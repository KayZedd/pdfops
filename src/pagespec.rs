//! Page selection syntax shared by every command.
//!
//! Comma separated parts, 1-based: `3`, `2-5`, `7-` (to end), `-4` (from start),
//! `5-2` (descending), `last`, `odd`, `even`, `all`.

use anyhow::{Result, bail};

/// Expands `spec` into page numbers, in the order written. Duplicates are kept.
pub fn parse(spec: &str, total: u32) -> Result<Vec<u32>> {
    if total == 0 {
        bail!("document has no pages");
    }
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "all" => out.extend(1..=total),
            "odd" => out.extend((1..=total).step_by(2)),
            "even" => out.extend((2..=total).step_by(2)),
            p => match p.split_once('-') {
                None => out.push(bound(p, total)?),
                Some((a, b)) => {
                    let start = if a.trim().is_empty() {
                        1
                    } else {
                        bound(a, total)?
                    };
                    let end = if b.trim().is_empty() {
                        total
                    } else {
                        bound(b, total)?
                    };
                    if start <= end {
                        out.extend(start..=end);
                    } else {
                        out.extend((end..=start).rev());
                    }
                }
            },
        }
    }
    if out.is_empty() {
        bail!("page spec '{spec}' selects no pages");
    }
    Ok(out)
}

/// `spec` if given, otherwise every page.
pub fn parse_or_all(spec: Option<&str>, total: u32) -> Result<Vec<u32>> {
    match spec {
        Some(s) => parse(s, total),
        None => parse("all", total),
    }
}

fn bound(s: &str, total: u32) -> Result<u32> {
    let s = s.trim();
    if s == "last" {
        return Ok(total);
    }
    let n: u32 = s
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid page '{s}'"))?;
    if n == 0 || n > total {
        bail!("page {n} out of range 1-{total}");
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn singles_ranges_and_keywords() {
        assert_eq!(parse("1,3-5", 10).unwrap(), [1, 3, 4, 5]);
        assert_eq!(parse("8-", 10).unwrap(), [8, 9, 10]);
        assert_eq!(parse("-2, last", 10).unwrap(), [1, 2, 10]);
        assert_eq!(parse("3-1", 10).unwrap(), [3, 2, 1]);
        assert_eq!(parse("odd", 5).unwrap(), [1, 3, 5]);
        assert_eq!(parse("even", 5).unwrap(), [2, 4]);
        assert_eq!(parse("all", 3).unwrap(), [1, 2, 3]);
        assert_eq!(parse("2,2", 3).unwrap(), [2, 2]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse("0", 5).is_err());
        assert!(parse("6", 5).is_err());
        assert!(parse("a-b", 5).is_err());
        assert!(parse("", 5).is_err());
        assert!(parse("even", 1).is_err());
        assert!(parse("1", 0).is_err());
    }
}
