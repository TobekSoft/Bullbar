//! Every scenario in `scenarios/`, on the sample configs in `config/` (TODO §1.7).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use bullbar_core::config::{Config, ConfigText};
use bullbar_core::scenario;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load(dir: &Path) -> Config {
    let read = |name: &str| fs::read_to_string(dir.join(name)).ok();
    let need = |name: &str| read(name).unwrap_or_else(|| panic!("{}/{name}", dir.display()));
    let (providers, policy, slots) = (
        need("providers.toml"),
        need("policy.toml"),
        need("slots.toml"),
    );
    let (user, ledger) = (read("user.toml"), read("ledger.toml"));
    Config::load(ConfigText {
        providers: &providers,
        policy: &policy,
        user: user.as_deref(),
        slots: &slots,
        ledger: ledger.as_deref(),
    })
    .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
}

#[test]
fn every_scenario_decides_as_expected() {
    let mut files: Vec<PathBuf> = fs::read_dir(repo().join("scenarios"))
        .expect("scenarios/ exists")
        .map(|entry| entry.expect("readable").path())
        .filter(|path| path.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    let mut configs = BTreeMap::new();
    let (mut ran, mut failed) = (0, Vec::new());
    for file in &files {
        let text = fs::read_to_string(file).expect("readable");
        let scenarios =
            scenario::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for s in scenarios {
            let config = configs
                .entry(s.edition.clone())
                .or_insert_with(|| load(&repo().join("config").join(&s.edition)));
            let checked = s.run(config);
            ran += 1;
            if !checked.passed() {
                let name = file.file_name().expect("a file").to_string_lossy();
                failed.push(format!(
                    "{name}: {}\n  {}\n{}",
                    checked.name,
                    checked.failures.join("\n  "),
                    checked.decision
                ));
            }
        }
    }
    assert!(ran >= 50, "only {ran} scenarios");
    assert!(
        failed.is_empty(),
        "{} of {ran} failed:\n\n{}",
        failed.len(),
        failed.join("\n")
    );
}
