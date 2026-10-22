//! Legacy transaction builder for measurement transactions.
//!
//! Hand-rolled wire format so the account layout (and therefore every write lock) is fully
//! explicit. `tests::matches_solana_message_crate` checks byte equality against the official
//! `solana-message` crate.
//!
//! Lock-free measurement tx (arms P, F, X, R):
//!   [SetComputeUnitLimit, SetComputeUnitPrice, SetLoadedAccountsDataSizeLimit,
//!    SystemTransfer(payer -> payer, nonce lamports)]
//!   Accounts: payer (writable signer), System, ComputeBudget (read-only). ONE writable account.
//! Conflict-chain tx (arm C): same, but SystemTransfer(payer -> shared_sink, 1 lamport).
//!   Accounts: payer (w,s), shared_sink (w), programs (ro). TWO writable accounts, one shared.

use slwlab_common::cost::TxShape;
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;

pub const COMPUTE_BUDGET_PROGRAM: Pubkey =
    Pubkey::from_str_const("ComputeBudget111111111111111111111111111111");
pub const SYSTEM_PROGRAM: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");

/// Loaded-accounts limit we always request: one 32 KiB page (8 CU of cost). Our txs load
/// only the payer (0 bytes), an optional 0-byte sink and two builtin program accounts.
pub const LOADED_ACCOUNTS_DATA_LIMIT: u32 = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TxParams {
    pub cu_limit: u32,
    pub cu_price_micro_lamports: u64,
    /// Makes otherwise-identical transactions unique; becomes the transfer amount for
    /// lock-free txs (self-transfer, so no value moves). Must be > 0.
    pub nonce: u64,
    /// `Some(sink)` builds an arm-C conflict-chain transaction.
    pub shared_sink: Option<Pubkey>,
}

#[derive(Debug, Clone)]
pub struct BuiltTx {
    pub signature: Signature,
    pub wire: Vec<u8>,
    pub message: Vec<u8>,
    pub shape: TxShape,
    pub writable_accounts: Vec<Pubkey>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BuildError {
    #[error("nonce must be > 0 (a zero-lamport self-transfer would collide across arms)")]
    ZeroNonce,
    #[error("cu_limit {0} outside 1..=1_400_000")]
    CuLimit(u32),
    #[error("shared sink must differ from the payer")]
    SinkIsPayer,
    #[error("transaction too large: {0} bytes > 1232")]
    TooLarge(usize),
}

pub fn compact_u16(mut v: usize, out: &mut Vec<u8>) {
    assert!(v <= 0xffff);
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        b |= 0x80;
        out.push(b);
    }
}

struct Ix {
    program_index: u8,
    accounts: Vec<u8>,
    data: Vec<u8>,
}

fn compute_budget_ixs(p: &TxParams, cb_index: u8) -> [Ix; 3] {
    let mut limit = vec![2u8];
    limit.extend_from_slice(&p.cu_limit.to_le_bytes());
    let mut price = vec![3u8];
    price.extend_from_slice(&p.cu_price_micro_lamports.to_le_bytes());
    let mut loaded = vec![4u8];
    loaded.extend_from_slice(&LOADED_ACCOUNTS_DATA_LIMIT.to_le_bytes());
    [limit, price, loaded].map(|data| Ix {
        program_index: cb_index,
        accounts: vec![],
        data,
    })
}

fn transfer_data(lamports: u64) -> Vec<u8> {
    let mut d = 2u32.to_le_bytes().to_vec();
    d.extend_from_slice(&lamports.to_le_bytes());
    d
}

/// Serialize a legacy message with the given (already ordered) account keys and header.
fn serialize_message(header: [u8; 3], keys: &[Pubkey], blockhash: &Hash, ixs: &[Ix]) -> Vec<u8> {
    let mut m = header.to_vec();
    compact_u16(keys.len(), &mut m);
    for k in keys {
        m.extend_from_slice(k.as_ref());
    }
    m.extend_from_slice(blockhash.as_ref());
    compact_u16(ixs.len(), &mut m);
    for ix in ixs {
        m.push(ix.program_index);
        compact_u16(ix.accounts.len(), &mut m);
        m.extend_from_slice(&ix.accounts);
        compact_u16(ix.data.len(), &mut m);
        m.extend_from_slice(&ix.data);
    }
    m
}

pub fn build(payer: &Keypair, blockhash: &Hash, p: &TxParams) -> Result<BuiltTx, BuildError> {
    if p.nonce == 0 {
        return Err(BuildError::ZeroNonce);
    }
    if p.cu_limit == 0 || p.cu_limit > 1_400_000 {
        return Err(BuildError::CuLimit(p.cu_limit));
    }
    let payer_pk = payer.pubkey();
    let (header, keys, transfer) = match p.shared_sink {
        None => (
            // 1 signer, 0 readonly signed, 2 readonly unsigned. Read-only keys are sorted by
            // pubkey like solana-message does (System 111.. sorts before ComputeBudget).
            [1u8, 0, 2],
            vec![payer_pk, SYSTEM_PROGRAM, COMPUTE_BUDGET_PROGRAM],
            Ix {
                program_index: 1,
                accounts: vec![0, 0],
                data: transfer_data(p.nonce),
            },
        ),
        Some(sink) => {
            if sink == payer_pk {
                return Err(BuildError::SinkIsPayer);
            }
            (
                [1u8, 0, 2],
                vec![payer_pk, sink, SYSTEM_PROGRAM, COMPUTE_BUDGET_PROGRAM],
                Ix {
                    program_index: 2,
                    accounts: vec![0, 1],
                    data: transfer_data(1),
                },
            )
        }
    };
    let cb_index = (keys.len() - 1) as u8;
    let [a, b, c] = compute_budget_ixs(p, cb_index);
    let ixs = [a, b, c, transfer];
    let instruction_data_len = ixs.iter().map(|i| i.data.len() as u64).sum();
    let message = serialize_message(header, &keys, blockhash, &ixs);
    let signature = payer.sign_message(&message);
    let mut wire = Vec::with_capacity(1 + 64 + message.len());
    compact_u16(1, &mut wire);
    wire.extend_from_slice(signature.as_ref());
    wire.extend_from_slice(&message);
    if wire.len() > 1232 {
        return Err(BuildError::TooLarge(wire.len()));
    }
    let n_writable = usize::from(header[0] - header[1])
        + (keys.len() - usize::from(header[0]) - usize::from(header[2]));
    let writable_accounts = keys[..n_writable].to_vec();
    Ok(BuiltTx {
        signature,
        wire,
        message,
        shape: TxShape {
            n_signatures: 1,
            n_writable_accounts: n_writable as u64,
            instruction_data_len,
            cu_limit: p.cu_limit,
            cu_price_micro_lamports: p.cu_price_micro_lamports,
            loaded_accounts_data_size_limit: Some(LOADED_ACCOUNTS_DATA_LIMIT),
        },
        writable_accounts,
    })
}

/// Plain funding/sweep transfer (not a measurement tx): payer -> to, no priority fee.
pub fn build_transfer(from: &Keypair, to: &Pubkey, lamports: u64, blockhash: &Hash) -> BuiltTx {
    let keys = vec![from.pubkey(), *to, SYSTEM_PROGRAM];
    let ix = Ix {
        program_index: 2,
        accounts: vec![0, 1],
        data: transfer_data(lamports),
    };
    let message = serialize_message([1, 0, 1], &keys, blockhash, std::slice::from_ref(&ix));
    let signature = from.sign_message(&message);
    let mut wire = vec![1u8];
    wire.extend_from_slice(signature.as_ref());
    wire.extend_from_slice(&message);
    BuiltTx {
        signature,
        wire,
        message,
        shape: TxShape {
            n_signatures: 1,
            n_writable_accounts: 2,
            instruction_data_len: 12,
            cu_limit: 0,
            cu_price_micro_lamports: 0,
            loaded_accounts_data_size_limit: None,
        },
        writable_accounts: keys[..2].to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use slwlab_common::fees::total_fee;

    fn params(price: u64, sink: Option<Pubkey>) -> TxParams {
        TxParams {
            cu_limit: 1000,
            cu_price_micro_lamports: price,
            nonce: 7,
            shared_sink: sink,
        }
    }

    #[test]
    fn compact_u16_encoding() {
        for (v, enc) in [
            (0usize, vec![0u8]),
            (0x7f, vec![0x7f]),
            (0x80, vec![0x80, 0x01]),
            (0x3fff, vec![0xff, 0x7f]),
            (0x4000, vec![0x80, 0x80, 0x01]),
        ] {
            let mut out = vec![];
            compact_u16(v, &mut out);
            assert_eq!(out, enc, "v={v}");
        }
    }

    #[test]
    fn lock_free_tx_has_exactly_one_writable_account_the_payer() {
        let kp = Keypair::new();
        let tx = build(
            &kp,
            &Hash::new_from_array(rand::random()),
            &params(10, None),
        )
        .unwrap();
        assert_eq!(tx.writable_accounts, vec![kp.pubkey()]);
        assert_eq!(tx.shape.n_writable_accounts, 1);
        assert_eq!(tx.shape.instruction_data_len, 5 + 9 + 5 + 12);
    }

    #[test]
    fn conflict_tx_writes_payer_and_shared_sink_only() {
        let kp = Keypair::new();
        let sink = Pubkey::new_unique();
        let tx = build(
            &kp,
            &Hash::new_from_array(rand::random()),
            &params(10, Some(sink)),
        )
        .unwrap();
        assert_eq!(tx.writable_accounts, vec![kp.pubkey(), sink]);
        assert_eq!(
            build(
                &kp,
                &Hash::new_from_array(rand::random()),
                &params(10, Some(kp.pubkey()))
            )
            .unwrap_err(),
            BuildError::SinkIsPayer
        );
    }

    #[test]
    fn signature_verifies_and_nonce_makes_txs_unique() {
        let kp = Keypair::new();
        let bh = Hash::new_from_array(rand::random());
        let a = build(&kp, &bh, &params(10, None)).unwrap();
        let mut p = params(10, None);
        p.nonce = 8;
        let b = build(&kp, &bh, &p).unwrap();
        assert!(a.signature.verify(kp.pubkey().as_ref(), &a.message));
        assert_ne!(a.signature, b.signature);
        assert_eq!(&a.wire[1..65], a.signature.as_ref());
        assert_eq!(
            build(&kp, &bh, &TxParams { nonce: 0, ..p }).unwrap_err(),
            BuildError::ZeroNonce
        );
    }

    #[test]
    fn matches_solana_message_crate() {
        use solana_instruction::{AccountMeta, Instruction};
        let kp = Keypair::new();
        let bh = Hash::new_from_array(rand::random());
        let sink = Pubkey::new_unique();
        for s in [None, Some(sink)] {
            let p = params(12_345, s);
            let ours = build(&kp, &bh, &p).unwrap();
            let mut ixs = vec![];
            for data in [
                [&[2u8][..], &p.cu_limit.to_le_bytes()].concat(),
                [&[3u8][..], &p.cu_price_micro_lamports.to_le_bytes()].concat(),
                [&[4u8][..], &LOADED_ACCOUNTS_DATA_LIMIT.to_le_bytes()].concat(),
            ] {
                ixs.push(Instruction::new_with_bytes(
                    COMPUTE_BUDGET_PROGRAM,
                    &data,
                    vec![],
                ));
            }
            let (to, amount) = match s {
                None => (kp.pubkey(), p.nonce),
                Some(sink) => (sink, 1),
            };
            ixs.push(Instruction::new_with_bytes(
                SYSTEM_PROGRAM,
                &transfer_data(amount),
                vec![
                    AccountMeta::new(kp.pubkey(), true),
                    AccountMeta::new(to, false),
                ],
            ));
            let msg = solana_message::Message::new_with_blockhash(&ixs, Some(&kp.pubkey()), &bh);
            assert_eq!(msg.serialize(), ours.message, "sink={s:?}");
        }
    }

    proptest! {
        #[test]
        fn shape_fee_matches_common_fee_math(price in 0u64..10_000_000, limit in 1u32..1_400_000) {
            let kp = Keypair::new();
            let p = TxParams { cu_limit: limit, cu_price_micro_lamports: price, nonce: 1, shared_sink: None };
            let tx = build(&kp, &Hash::default(), &p).unwrap();
            prop_assert_eq!(tx.shape.priority_fee() + 5000, total_fee(5000, 1, price, limit));
            prop_assert!(tx.wire.len() <= 1232);
        }
    }
}
