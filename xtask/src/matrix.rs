use crate::combo::{Combo, Task};

/// Every feature combination the workspace claims to support.
///
/// The curve backends (`k256` / `secp256k1`) and the JSON backends
/// (`serde` / `bourne`) are each mutually exclusive, so the supported set is
/// their product. A combination that no command ever builds is a
/// combination that breaks without anyone noticing.
pub struct Matrix {
    combos: Vec<Combo>,
}

impl Default for Matrix {
    fn default() -> Self {
        Self::new()
    }
}

impl Matrix {
    pub fn new() -> Self {
        let mut combos = Vec::new();

        for json in ["serde", "bourne"] {
            for curve in ["k256", "secp256k1"] {
                combos.push(Combo::new("nostro2", &[json, curve]));
                combos.push(Combo::new("nostro2-signer", &[json, curve]));
                combos.push(Combo::new("nostro2-nips", &[json, curve]));
                // `rustls-custom-provider` links no provider and takes one
                // from the caller. It is a third TLS choice, not an absence
                // of one, so it belongs here beside the built-in providers.
                for tls in ["rustls-ring", "rustls-aws-lc", "rustls-custom-provider"] {
                    combos.push(Combo::new("nostro2-relay", &[tls, json, curve]));
                }
            }
        }

        Self { combos }
    }

    #[cfg(test)]
    pub fn combos(&self) -> &[Combo] {
        &self.combos
    }

    /// Every combination paired with every task, in run order.
    pub fn jobs(&self) -> Vec<(Combo, Task)> {
        let mut jobs = Vec::with_capacity(self.combos.len() * 2);
        for combo in &self.combos {
            jobs.push((combo.clone(), Task::Clippy));
            jobs.push((combo.clone(), Task::Test));
        }
        jobs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_matrix_covers_both_curves_for_every_package() {
        let matrix = Matrix::new();
        for package in [
            "nostro2",
            "nostro2-signer",
            "nostro2-nips",
            "nostro2-relay",
        ] {
            for curve in ["k256", "secp256k1"] {
                assert!(
                    matrix.combos().iter().any(|c| c.package() == package
                        && c.feature_list().contains(curve)),
                    "{package} never gets built with {curve}, so {curve} can rot unnoticed"
                );
            }
        }
    }

    #[test]
    fn the_matrix_covers_both_json_backends_for_every_package() {
        let matrix = Matrix::new();
        for package in [
            "nostro2",
            "nostro2-signer",
            "nostro2-nips",
            "nostro2-relay",
        ] {
            for json in ["serde", "bourne"] {
                assert!(
                    matrix.combos().iter().any(|c| c.package() == package
                        && c.feature_list().contains(json)),
                    "{package} never gets built with {json}"
                );
            }
        }
    }

    #[test]
    fn the_relay_covers_every_tls_choice() {
        let matrix = Matrix::new();
        for tls in ["rustls-ring", "rustls-aws-lc", "rustls-custom-provider"] {
            assert!(
                matrix
                    .combos()
                    .iter()
                    .any(|c| c.package() == "nostro2-relay" && c.feature_list().contains(tls)),
                "the relay never gets built with {tls}"
            );
        }
    }

    #[test]
    fn no_combination_enables_a_provider_alongside_the_custom_one() {
        for combo in Matrix::new().combos() {
            let features = combo.feature_list();
            if !features.contains("rustls-custom-provider") {
                continue;
            }
            assert!(
                !features.contains("rustls-ring") && !features.contains("rustls-aws-lc"),
                "{} links a provider while claiming the caller supplies one",
                combo.label()
            );
        }
    }

    #[test]
    fn no_combination_enables_two_curves_at_once() {
        for combo in Matrix::new().combos() {
            let features = combo.feature_list();
            assert!(
                !(features.contains("k256") && features.contains("secp256k1")),
                "{} enables two mutually exclusive curves",
                combo.label()
            );
        }
    }

    #[test]
    fn no_combination_enables_two_json_backends_at_once() {
        for combo in Matrix::new().combos() {
            let features = combo.feature_list();
            assert!(
                !(features.contains("serde") && features.contains("bourne")),
                "{} enables two mutually exclusive JSON backends",
                combo.label()
            );
        }
    }

    #[test]
    fn every_combination_runs_both_tasks() {
        let matrix = Matrix::new();
        assert_eq!(matrix.jobs().len(), matrix.combos().len() * 2);
    }
}
