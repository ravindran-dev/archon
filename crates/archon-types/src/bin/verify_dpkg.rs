use archon_types::Version;
use std::process::Command;
use std::cmp::Ordering;

fn main() {
    let pairs = vec![
        ("1.0~rc1", "1.0"),
        ("1.0~rc1", "1.0.1"),
        ("1.0~rc1", "1.0~rc2"),
        ("1.0~rc2", "1.0~rc1"),
        ("1.0~rc1~2", "1.0~rc1"),
        ("1~rc1", "1~rc1"),
        ("1.0~~", "1.0~"),
        ("1:2.0", "2.0~rc1"),
        ("1.0", "1.0~rc1"),
        ("1.0-1", "1.0-2"),
        ("1.0a", "1.0"),
        ("1.0-1ubuntu1", "1.0-1"),
        ("1.0-1ubuntu1", "1.0-1ubuntu2"),
        ("2.0-1", "1:1.0-1"),
        ("1.0-1", "1.0-1+b1"),
        ("1.0+nmu1", "1.0"),
        ("0.99", "1.0~alpha"),
        ("1.0~alpha", "1.0~beta"),
        ("1.0~beta", "1.0~rc"),
        ("1.0~rc", "1.0"),
        ("1.0.0", "1.0"),
        ("1.0", "1.0.0"), // dpkg treats these as equal? or not?
        ("1.0-1", "1.0-0.1"),
        ("1.0-1~bpo10+1", "1.0-1"),
        ("1.2.3-4", "1.2.3-4"),
    ];

    let mut mismatches = 0;

    for (a, b) in pairs {
        let rust_a = Version::parse_dpkg(a).unwrap();
        let rust_b = Version::parse_dpkg(b).unwrap();
        let rust_ord = rust_a.vercmp(&rust_b);

        let lt = Command::new("dpkg")
            .arg("--compare-versions").arg(a).arg("lt").arg(b)
            .status().unwrap().success();
        let gt = Command::new("dpkg")
            .arg("--compare-versions").arg(a).arg("gt").arg(b)
            .status().unwrap().success();
        let eq = Command::new("dpkg")
            .arg("--compare-versions").arg(a).arg("eq").arg(b)
            .status().unwrap().success();

        let real_ord = if lt {
            Ordering::Less
        } else if gt {
            Ordering::Greater
        } else if eq {
            Ordering::Equal
        } else {
            panic!("dpkg compare-versions failed for {} and {}", a, b);
        };

        if rust_ord != real_ord {
            println!("MISMATCH: {} vs {} -> Rust says {:?}, dpkg says {:?}", a, b, rust_ord, real_ord);
            mismatches += 1;
        } else {
            println!("MATCH: {} vs {} -> {:?}", a, b, real_ord);
        }
    }

    if mismatches == 0 {
        println!("ALL MATCH!");
    } else {
        println!("{} mismatches found.", mismatches);
    }
}
