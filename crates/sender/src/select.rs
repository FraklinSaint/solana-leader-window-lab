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
    let (jito, bam) = (
        k.running_jito.unwrap_or(false),
        k.running_bam.unwrap_or(false),
    );
    let agrees = match s {
        Stratum::JitoAgave => jito && !bam,
        Stratum::BamAgave | Stratum::BamFiredancer => bam,
        Stratum::Agave | Stratum::Firedancer => !jito && !bam,
        // Harmonic and Rakurai are not Jito products; Kobe can only say "not BAM".
        Stratum::HarmonicAgave | Stratum::HarmonicFiredancer | Stratum::Rakurai => !bam,
        Stratum::Unknown => false,
    };
    if agrees {
        (s, Confidence::High, ev)
    } else {
        ev.push("Kobe disagrees with clientId".into());
        (s, Confidence::Low, ev)
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
}
