//! Small helpers shared across modules.

use url::Url;

/// Port of Go's `path.Clean`.
fn path_clean(p: &str) -> String {
    if p.is_empty() {
        return ".".to_string();
    }
    let rooted = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();

    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => match out.last() {
                Some(&last) if last != ".." => {
                    out.pop();
                }
                _ => {
                    if !rooted {
                        out.push("..");
                    }
                }
            },
            s => out.push(s),
        }
    }

    let joined = out.join("/");
    match (rooted, joined.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// Port of Go's `path.Join`.
pub fn path_join(a: &str, b: &str) -> String {
    let joined = match (a.is_empty(), b.is_empty()) {
        (true, true) => return String::new(),
        (true, false) => b.to_string(),
        (false, true) => a.to_string(),
        (false, false) => format!("{a}/{b}"),
    };
    path_clean(&joined)
}

/// Appends `sub` to the URL's path using Go's `path.Join` semantics.
///
/// Go's `url.URL.String()` inserts a leading slash when the path is relative and
/// a host is present, so a server URL without a path still yields `/empty.php`.
pub fn url_join_path(base: &Url, sub: &str) -> Url {
    let mut u = base.clone();
    let joined = path_join(base.path(), sub);
    let joined = if joined.is_empty() {
        String::new()
    } else if joined.starts_with('/') {
        joined
    } else {
        format!("/{joined}")
    };
    u.set_path(&joined);
    u
}

/// Returns the average of a slice, or 0.0 when empty.
pub fn avg(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    vals.iter().sum::<f64>() / vals.len() as f64
}

/// Rounds to two decimal places, matching the Go version's `math.Round(x*100)/100`.
pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_join_matches_go() {
        assert_eq!(path_join("", "empty.php"), "empty.php");
        assert_eq!(path_join("/backend", "empty.php"), "/backend/empty.php");
        assert_eq!(path_join("/backend/", "/empty.php"), "/backend/empty.php");
        assert_eq!(path_join("/a/b", "../c"), "/a/c");
        assert_eq!(path_join("", ""), "");
        assert_eq!(path_join("/", ""), "/");
    }

    #[test]
    fn url_join_adds_leading_slash() {
        let base = Url::parse("https://example.com").unwrap();
        assert_eq!(
            url_join_path(&base, "empty.php").as_str(),
            "https://example.com/empty.php"
        );
        let base = Url::parse("https://example.com/backend").unwrap();
        assert_eq!(
            url_join_path(&base, "garbage.php").as_str(),
            "https://example.com/backend/garbage.php"
        );
    }

    #[test]
    fn round2_matches_go() {
        assert_eq!(round2(199.804), 199.8);
        assert_eq!(round2(5.455), 5.46);
    }
}
