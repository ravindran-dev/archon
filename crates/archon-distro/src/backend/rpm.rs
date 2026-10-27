//! Stable RPM query-format protocol shared only by RPM-family adapters.
use super::InstalledPackage;

pub(super) fn inventory_argv() -> Vec<String> {
    [
        "rpm",
        "-qa",
        "--queryformat",
        "%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}\t%{ARCH}\n",
    ]
    .map(str::to_owned)
    .to_vec()
}

pub(super) fn parse_inventory(output: &str) -> Result<Vec<InstalledPackage>, String> {
    let mut packages = Vec::new();
    for (index, line) in output.lines().enumerate() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 || fields.iter().any(|field| field.is_empty()) {
            return Err(format!(
                "invalid RPM inventory record at line {}",
                index + 1
            ));
        }
        packages.push(InstalledPackage {
            name: fields[0].into(),
            version: fields[1].into(),
            architecture: fields[2].into(),
        });
    }
    packages.sort_by(|a, b| {
        (&a.name, &a.architecture, &a.version).cmp(&(&b.name, &b.architecture, &b.version))
    });
    Ok(packages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_preserves_epoch_and_multilib_and_sorts_deterministically() {
        let packages = parse_inventory(
            "zlib\t0:1.3-1.fc43\tx86_64\nzlib\t0:1.3-1.fc43\ti686\nbash\t2:5.3-1.fc43\tx86_64\n",
        )
        .unwrap();
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].version, "2:5.3-1.fc43");
        assert_eq!(packages[1].architecture, "i686");
        assert_eq!(packages[2].architecture, "x86_64");
    }

    #[test]
    fn malformed_records_fail_instead_of_silently_dropping_packages() {
        for bad in ["bash\n", "bash\t\tx86_64\n", "bash\t1\tx86_64\textra\n"] {
            assert!(parse_inventory(bad).is_err());
        }
        assert!(parse_inventory("").unwrap().is_empty());
    }
}
