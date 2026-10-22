//! Validator selection: client label + evidence + confidence for every voting validator,
//! upcoming-window counts, and a stratified pick. Conclusions are per validator; the label
//! only decides which stratum a validator represents.
//!
//! Required strata (must be filled): Agave, JitoAgave, Firedancer.
//! Upside strata (best effort): HarmonicAgave, HarmonicFiredancer, BamAgave, BamFiredancer, Rakurai.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Stratum {
    Agave,
    JitoAgave,
    Firedancer,
    HarmonicAgave,
    HarmonicFiredancer,
    BamAgave,
    BamFiredancer,
    Rakurai,
    Unknown,
}

impl Stratum {
    pub const REQUIRED: [Stratum; 3] = [Stratum::Agave, Stratum::JitoAgave, Stratum::Firedancer];

    pub fn from_client_id(client_id: Option<&str>) -> Stratum {
        match client_id {
            Some("Agave") => Stratum::Agave,
            Some("JitoLabs") => Stratum::JitoAgave,
            Some("Firedancer") => Stratum::Firedancer,
            Some("HarmonicAgave") => Stratum::HarmonicAgave,
            Some("HarmonicFiredancer") => Stratum::HarmonicFiredancer,
            Some("AgaveBam") => Stratum::BamAgave,
            Some("FireBAM") => Stratum::BamFiredancer,
            Some("Rakurai") => Stratum::Rakurai,
            _ => Stratum::Unknown,
        }
    }

    fn is_firedancer_family(self) -> bool {
        matches!(
            self,
            Stratum::Firedancer | Stratum::HarmonicFiredancer | Stratum::BamFiredancer
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KobeInfo {
    pub running_jito: Option<bool>,
    pub running_bam: Option<bool>,
    pub bam_connection_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub identity: String,
    pub vote_account: String,
    pub stake_lamports: u64,
    pub client_id: Option<String>,
    pub version: Option<String>,
    pub tpu_quic: Option<String>,
    pub stratum: Stratum,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
    pub kobe: Option<KobeInfo>,
    pub windows_in_horizon: usize,
}

/// Agave-family versions are `4.x.y`; Firedancer uses its own scheme (`26.10.4`, `0.x`).
fn version_family(version: Option<&str>) -> Option<&'static str> {
    let major: u64 = version?.split('.').next()?.parse().ok()?;
    Some(if (1..=9).contains(&major) {
        "agave"
    } else {
        "firedancer"
    })
}

/// Label one validator. `high` needs an independent source (Kobe) to agree with the
/// self-reported gossip `clientId`; `medium` = clientId and version scheme agree; `low` otherwise.
pub fn label(
    client_id: Option<&str>,
    version: Option<&str>,
    kobe: Option<&KobeInfo>,
) -> (Stratum, Confidence, Vec<String>) {
    let s = Stratum::from_client_id(client_id);
    let mut ev = vec![
        format!("gossip clientId={client_id:?}"),
        format!("version={version:?}"),
    ];
    let fam = version_family(version);
    let family_ok = match fam {
        Some("firedancer") => s.is_firedancer_family(),
        Some(_) => !s.is_firedancer_family() && s != Stratum::Unknown,
        None => false,
    };
    if !family_ok {
        ev.push(format!("version family {fam:?} inconsistent with clientId"));
        return (s, Confidence::Low, ev);
    }
    let Some(k) = kobe else {
        ev.push("not listed by Kobe".into());
        return (s, Confidence::Medium, ev);
    };
    ev.push(format!(
        "kobe running_jito={:?} running_bam={:?} bam_connection_rate={:?}",
        k.running_jito, k.running_bam, k.bam_connection_rate
    ));
    // Some(true) = Kobe confirms, Some(false) = Kobe contradicts, None = Kobe has no value
    // for the field that would decide (a missing field is not a contradiction).
    let (jito, bam) = (k.running_jito, k.running_bam);
    let agrees = match s {
        Stratum::JitoAgave => jito.map(|j| j && bam != Some(true)),
        Stratum::BamAgave | Stratum::BamFiredancer => bam,
        // Vanilla Agave has no block-engine client: any Jito/BAM flag contradicts it.
        Stratum::Agave => match (jito, bam) {
            (Some(true), _) | (_, Some(true)) => Some(false),
            (Some(false), _) => Some(true),
            (None, _) => None,
        },
        // Firedancer ships its own Jito bundle tile (fd_bundle_tile.c:172, VERIFIED_FACTS F7),
        // so running_jito says nothing about the client; only BAM would contradict it.
        // Harmonic and Rakurai are not Jito products either; Kobe can only say "not BAM".
        Stratum::Firedancer
        | Stratum::HarmonicAgave
        | Stratum::HarmonicFiredancer
        | Stratum::Rakurai => bam.map(|b| !b),
        Stratum::Unknown => Some(false),
    };
    if s == Stratum::Firedancer && jito == Some(true) {
        ev.push("running_jito: Firedancer bundle tile enabled (bundles are out of scope)".into());
    }
    match agrees {
        Some(true) => (s, Confidence::High, ev),
        None => {
            ev.push("Kobe lists the validator but has no deciding field".into());
            (s, Confidence::Medium, ev)
        }
        Some(false) => {
            ev.push("Kobe disagrees with clientId".into());
            (s, Confidence::Low, ev)
        }
    }
}

pub fn quotas() -> BTreeMap<Stratum, usize> {
    BTreeMap::from([
        (Stratum::Agave, 2),
        (Stratum::JitoAgave, 2),
        (Stratum::Firedancer, 2),
        (Stratum::HarmonicAgave, 1),
        (Stratum::HarmonicFiredancer, 1),
        (Stratum::BamAgave, 1),
        (Stratum::Rakurai, 1),
    ])
}

/// Pick per stratum: confidence >= medium, has tpuQuic, >= `min_windows` upcoming windows,
/// highest window count first (ties: identity, for determinism).
pub fn pick(
    cands: &[Candidate],
    quotas: &BTreeMap<Stratum, usize>,
    min_windows: usize,
) -> (Vec<Candidate>, Vec<String>) {
    let mut by: HashMap<Stratum, Vec<&Candidate>> = HashMap::new();
    for c in cands {
        if c.confidence >= Confidence::Medium
            && c.tpu_quic.is_some()
            && c.windows_in_horizon >= min_windows
        {
            by.entry(c.stratum).or_default().push(c);
        }
    }
    let mut picked = vec![];
    let mut notes = vec![];
    for (s, &q) in quotas {
        let mut v = by.remove(s).unwrap_or_default();
        v.sort_by(|a, b| {
            b.windows_in_horizon
                .cmp(&a.windows_in_horizon)
                .then(b.confidence.cmp(&a.confidence))
                .then(a.identity.cmp(&b.identity))
        });
        if v.len() < q {
            let level = if Stratum::REQUIRED.contains(s) {
                "REQUIRED"
            } else {
                "upside"
            };
            notes.push(format!(
                "{level} stratum {s:?}: only {} of {q} eligible",
                v.len()
            ));
        }
        picked.extend(v.into_iter().take(q).cloned());
    }
    (picked, notes)
}

/// Kobe validator list keyed by identity (VERIFIED_FACTS F7). Optional source: callers
/// record a failure instead of aborting.
pub fn kobe_by_identity(v: &serde_json::Value) -> HashMap<String, KobeInfo> {
    v["validators"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some((
                        x["identity_account"].as_str()?.to_string(),
                        KobeInfo {
                            running_jito: x["running_jito"].as_bool(),
                            running_bam: x["running_bam"].as_bool(),
                            bam_connection_rate: x["bam_connection_rate"].as_f64(),
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Leader windows per identity with first slot >= `from_slot`. `schedule` is
/// getLeaderSchedule's identity -> slot indices relative to `epoch_start`; consecutive
/// indices form one window.
pub fn count_windows(
    schedule: &HashMap<String, Vec<u64>>,
    epoch_start: u64,
    from_slot: u64,
) -> HashMap<String, usize> {
    schedule
        .iter()
        .map(|(id, idxs)| {
            let mut v = idxs.clone();
            v.sort_unstable();
            let n = v
                .iter()
                .enumerate()
                .filter(|&(i, &x)| (i == 0 || v[i - 1] + 1 != x) && epoch_start + x >= from_slot)
                .count();
            (id.clone(), n)
        })
        .collect()
}

/// One candidate per voting (non-delinquent) validator.
pub fn candidates(
    nodes: &[crate::rpc::ClusterNode],
    votes: &crate::rpc::VoteAccounts,
    kobe: &HashMap<String, KobeInfo>,
    windows: &HashMap<String, usize>,
) -> Vec<Candidate> {
    let by_id: HashMap<&str, &crate::rpc::ClusterNode> =
        nodes.iter().map(|n| (n.pubkey.as_str(), n)).collect();
    let mut out: Vec<Candidate> = votes
        .current
        .iter()
        .map(|va| {
            let node = by_id.get(va.node_pubkey.as_str());
            let client_id = node.and_then(|n| n.client_id.clone());
            let version = node.and_then(|n| n.version.clone());
            let k = kobe.get(&va.node_pubkey);
            let (stratum, confidence, mut evidence) =
                label(client_id.as_deref(), version.as_deref(), k);
            if node.is_none() {
                evidence.push("not in getClusterNodes".into());
            }
            Candidate {
                identity: va.node_pubkey.clone(),
                vote_account: va.vote_pubkey.clone(),
                stake_lamports: va.activated_stake,
                client_id,
                version,
                tpu_quic: node.and_then(|n| n.tpu_quic.clone()),
                stratum,
                confidence,
                evidence,
                kobe: k.cloned(),
                windows_in_horizon: windows.get(&va.node_pubkey).copied().unwrap_or(0),
            }
        })
        .collect();
    out.sort_by(|a, b| a.identity.cmp(&b.identity));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kobe(jito: bool, bam: bool) -> KobeInfo {
        KobeInfo {
            running_jito: Some(jito),
            running_bam: Some(bam),
            bam_connection_rate: None,
        }
    }

    #[test]
    fn labels_and_confidence() {
        let (s, c, _) = label(Some("JitoLabs"), Some("4.3.2"), Some(&kobe(true, false)));
        assert_eq!((s, c), (Stratum::JitoAgave, Confidence::High));
        let (s, c, _) = label(Some("JitoLabs"), Some("4.3.2"), Some(&kobe(true, true)));
        assert_eq!((s, c), (Stratum::JitoAgave, Confidence::Low));
        let (s, c, _) = label(Some("Firedancer"), Some("26.10.4"), None);
        assert_eq!((s, c), (Stratum::Firedancer, Confidence::Medium));
        let (_, c, _) = label(Some("Firedancer"), Some("4.3.2"), None);
        assert_eq!(c, Confidence::Low);
        // Firedancer runs Jito bundles natively: not a contradiction
        let (s, c, _) = label(
            Some("Firedancer"),
            Some("26.10.4"),
            Some(&kobe(true, false)),
        );
        assert_eq!((s, c), (Stratum::Firedancer, Confidence::High));
        let (_, c, _) = label(Some("Firedancer"), Some("26.10.4"), Some(&kobe(true, true)));
        assert_eq!(c, Confidence::Low);
        // Kobe without a running_bam value: unknown, not a contradiction
        let no_bam = KobeInfo {
            running_jito: Some(true),
            running_bam: None,
            bam_connection_rate: None,
        };
        let (_, c, _) = label(Some("AgaveBam"), Some("4.3.2"), Some(&no_bam));
        assert_eq!(c, Confidence::Medium);
        let (_, c, _) = label(Some("Agave"), Some("4.3.2"), Some(&no_bam));
        assert_eq!(c, Confidence::Low);
        let (s, c, _) = label(Some("Unknown(5960)"), Some("4.3.2"), None);
        assert_eq!((s, c), (Stratum::Unknown, Confidence::Low));
    }

    #[test]
    fn pick_respects_quotas_and_reports_shortfalls() {
        let mk = |id: &str, s: Stratum, w: usize| Candidate {
            identity: id.into(),
            vote_account: format!("v{id}"),
            stake_lamports: 0,
            client_id: None,
            version: None,
            tpu_quic: Some("1.2.3.4:8009".into()),
            stratum: s,
            confidence: Confidence::High,
            evidence: vec![],
            kobe: None,
            windows_in_horizon: w,
        };
        let c = vec![
            mk("a", Stratum::Agave, 50),
            mk("b", Stratum::Agave, 80),
            mk("c", Stratum::Agave, 10),
            mk("j", Stratum::JitoAgave, 99),
        ];
        let (p, notes) = pick(&c, &quotas(), 40);
        let ids: Vec<_> = p.iter().map(|c| c.identity.as_str()).collect();
        assert_eq!(ids, vec!["b", "a", "j"]);
        assert!(
            notes
                .iter()
                .any(|n| n.contains("REQUIRED stratum JitoAgave"))
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("REQUIRED stratum Firedancer"))
        );
    }

    #[test]
    fn counts_windows_after_a_slot() {
        let sched = HashMap::from([
            (
                "a".to_string(),
                vec![0, 1, 2, 3, 8, 9, 10, 11, 12, 13, 14, 15, 40, 41, 42, 43],
            ),
            ("b".to_string(), vec![4, 5, 6, 7]),
        ]);
        let c = count_windows(&sched, 1_000, 1_000);
        assert_eq!((c["a"], c["b"]), (3, 1)); // 8..=15 is one merged window
        let c = count_windows(&sched, 1_000, 1_005);
        assert_eq!((c["a"], c["b"]), (2, 0));
    }

    #[test]
    fn kobe_parsing_and_candidates() {
        let k = kobe_by_identity(&serde_json::json!({"validators": [
            {"identity_account": "J", "running_jito": true, "running_bam": false},
            {"vote_account": "no identity"}
        ]}));
        assert_eq!(k.len(), 1);
        let nodes = vec![crate::rpc::ClusterNode {
            pubkey: "J".into(),
            version: Some("4.3.2".into()),
            feature_set: None,
            client_id: Some("JitoLabs".into()),
            tpu_quic: Some("1.2.3.4:8009".into()),
            shred_version: None,
        }];
        let votes = crate::rpc::VoteAccounts {
            current: vec![
                crate::rpc::VoteAccount {
                    vote_pubkey: "vJ".into(),
                    node_pubkey: "J".into(),
                    activated_stake: 5,
                    commission: 0,
                },
                crate::rpc::VoteAccount {
                    vote_pubkey: "vX".into(),
                    node_pubkey: "X".into(),
                    activated_stake: 1,
                    commission: 0,
                },
            ],
            delinquent: vec![],
        };
        let w = HashMap::from([("J".to_string(), 50)]);
        let c = candidates(&nodes, &votes, &k, &w);
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0].stratum, c[0].confidence),
            (Stratum::JitoAgave, Confidence::High)
        );
        assert_eq!(c[0].windows_in_horizon, 50);
        assert_eq!(c[1].confidence, Confidence::Low);
        assert!(
            c[1].evidence
                .iter()
                .any(|e| e.contains("not in getClusterNodes"))
        );
    }
}
