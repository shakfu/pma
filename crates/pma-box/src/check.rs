//! Character-set checks on every value that names a file, reaches a shell or a Containerfile, or
//! is handed to an engine. Each is ASCII-only: a byte outside its set fails it.

/// `s` is non-empty and its first byte passes `first`, every later byte `rest`.
fn shaped(s: &str, first: fn(u8) -> bool, rest: fn(u8) -> bool) -> bool {
    let mut bytes = s.bytes();
    bytes.next().is_some_and(first) && bytes.all(rest)
}

/// `s` is non-empty and every byte passes `ok`.
fn all(s: &str, ok: fn(u8) -> bool) -> bool {
    shaped(s, ok, ok)
}

fn lower_digit(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

fn hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// A catalogue name: an agent, recipe, kit, section or skill.
pub fn name(s: &str) -> bool {
    shaped(s, lower_digit, |b| lower_digit(b) || b == b'-')
}

pub fn sha256(s: &str) -> bool {
    s.len() == 64 && all(s, hex)
}

/// A full git object id: SHA-1 or SHA-256.
pub fn git_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && all(s, hex)
}

/// A login name for the image's agent user.
pub fn user(s: &str) -> bool {
    shaped(
        s,
        |b| b.is_ascii_lowercase() || b == b'_',
        |b| lower_digit(b) || b == b'_' || b == b'-',
    )
}

pub fn env_key(s: &str) -> bool {
    shaped(
        s,
        |b| b.is_ascii_alphabetic() || b == b'_',
        |b| b.is_ascii_alphanumeric() || b == b'_',
    )
}

/// A package spec: nothing a shell treats specially, and no leading dash, so a spec cannot become
/// a flag.
pub fn spec(s: &str) -> bool {
    shaped(
        s,
        |b| b.is_ascii_alphanumeric() || b == b'@',
        |b| b.is_ascii_alphanumeric() || b"._+:@/~=<>!,-".contains(&b),
    )
}

/// `pkg@1.2.3` or `@scope/pkg@1.2.3`.
pub fn npm_pinned(s: &str) -> bool {
    fn part(b: u8) -> bool {
        lower_digit(b) || b"._-".contains(&b)
    }
    let rest = match s.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, rest)) if all(scope, part) => rest,
            _ => return false,
        },
        None => s,
    };
    rest.split_once('@').is_some_and(|(name, version)| {
        all(name, part)
            && shaped(
                version,
                |b| b.is_ascii_digit(),
                |b| b.is_ascii_alphanumeric() || b"._+-".contains(&b),
            )
    })
}

/// `pkg==1.2.3`.
pub fn pip_pinned(s: &str) -> bool {
    s.split_once("==").is_some_and(|(name, version)| {
        shaped(
            name,
            |b| b.is_ascii_alphanumeric(),
            |b| b.is_ascii_alphanumeric() || b"._-".contains(&b),
        ) && all(version, |b| {
            b.is_ascii_alphanumeric() || b".+!_-".contains(&b)
        })
    })
}

/// `-f`, `--flag` or `--flag=value`.
pub fn flag(s: &str) -> bool {
    let Some(s) = s.strip_prefix('-') else {
        return false;
    };
    let s = s.strip_prefix('-').unwrap_or(s);
    let (name, value) = match s.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (s, None),
    };
    shaped(
        name,
        |b| b.is_ascii_lowercase(),
        |b| lower_digit(b) || b == b'-',
    ) && value.is_none_or(|v| all(v, |b| b.is_ascii_alphanumeric() || b"._/:-".contains(&b)))
}

pub fn url(s: &str) -> bool {
    s.strip_prefix("https://").is_some_and(|rest| {
        all(rest, |b| {
            b.is_ascii_alphanumeric() || b"._~:/?#@!$&'()*+,;=%-".contains(&b)
        })
    })
}

/// A relative path of non-empty segments.
pub fn segments(s: &str) -> bool {
    s.split('/')
        .all(|seg| all(seg, |b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)))
}

/// An octal file mode: `644` or `0644`.
pub fn mode(s: &str) -> bool {
    let s = if s.len() == 4 {
        s.strip_prefix('0').unwrap_or("")
    } else {
        s
    };
    s.len() == 3 && all(s, |b| (b'0'..=b'7').contains(&b))
}

/// `UID` or `UID:GID`.
pub fn uid(s: &str) -> bool {
    let digits = |p: &str| all(p, |b| b.is_ascii_digit());
    match s.split_once(':') {
        Some((u, g)) => digits(u) && digits(g),
        None => digits(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    /// Each check beside the pattern it replaced, and inputs at its edges. `uid` used `\d`, which
    /// in `regex` matches every Unicode digit; the check takes ASCII only, as an engine does.
    #[allow(clippy::type_complexity)]
    const CASES: &[(&str, fn(&str) -> bool, &str, &[&str])] = &[
        (
            "name",
            name,
            r"^[a-z0-9][a-z0-9-]*$",
            &["claude", "a-1", "-a", "A"],
        ),
        (
            "sha256",
            sha256,
            r"^[0-9a-f]{64}$",
            &["0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"],
        ),
        (
            "git_oid",
            git_oid,
            "^[0-9a-f]{40}([0-9a-f]{24})?$",
            &[
                "0123456789abcdef0123456789abcdef01234567",
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ],
        ),
        (
            "user",
            user,
            r"^[a-z_][a-z0-9_-]*$",
            &["agent", "_a-1", "1a"],
        ),
        (
            "env_key",
            env_key,
            r"^[A-Za-z_][A-Za-z0-9_]*$",
            &["PATH", "_x1", "1X"],
        ),
        (
            "spec",
            spec,
            r"^[A-Za-z0-9@][A-Za-z0-9._+:@/~=<>!,-]*$",
            &["@a/b@1.2", "pkg>=1,<2", "-rf"],
        ),
        (
            "npm_pinned",
            npm_pinned,
            r"^(@[a-z0-9._-]+/)?[a-z0-9._-]+@[0-9][A-Za-z0-9._+-]*$",
            &[
                "pkg@1.2.3",
                "@scope/pkg@1.2.3-rc.1",
                "@/p@1",
                "@s/p",
                "p@x",
                "@s/a/b@1",
                "a@1@2",
            ],
        ),
        (
            "pip_pinned",
            pip_pinned,
            r"^[A-Za-z0-9][A-Za-z0-9._-]*==[A-Za-z0-9.+!_-]+$",
            &["pkg==1.2.3", "a==1==2", "a=b==1", "Pkg_x==1!2+l"],
        ),
        (
            "flag",
            flag,
            r"^--?[a-z][a-z0-9-]*(=[A-Za-z0-9._/:-]+)?$",
            &["-y", "--no-x=/a:b", "---x", "--x=", "--x=a=b", "-"],
        ),
        (
            "url",
            url,
            r"^https://[A-Za-z0-9._~:/?#@!$&'()*+,;=%-]+$",
            &["https://x.io/a?b=c#d", "https://", "http://x"],
        ),
        (
            "segments",
            segments,
            r"^[A-Za-z0-9._+-]+(/[A-Za-z0-9._+-]+)*$",
            &["a/b.c", "a//b", "/a", "a/", ""],
        ),
        (
            "mode",
            mode,
            r"^0?[0-7]{3}$",
            &["644", "0644", "0000", "1644", "64", "00644", "8"],
        ),
        (
            "uid",
            uid,
            r"^[0-9]+(:[0-9]+)?$",
            &["1000", "1000:1000", "1:2:3", ":1", "1:"],
        ),
    ];

    /// Bytes from every class above, and from none: space, newline, multibyte UTF-8.
    const ALPHABET: &[&str] = &[
        "a", "f", "g", "z", "A", "Z", "0", "1", "7", "8", "9", "@", "/", ".", "_", "-", "+", ":",
        "=", "~", "<", ">", "!", ",", "?", "#", "$", "&", "'", "(", ")", "*", ";", "%", " ", "\n",
        "\\", "\"", "`", "é", "\u{0661}", "https://", "==", "--",
    ];

    /// xorshift64: deterministic, so a failure reproduces.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    /// Up to three edits of `seed`: insert, delete or replace a piece.
    fn mutate(rng: &mut Rng, seed: &str) -> String {
        let mut parts: Vec<String> = seed.chars().map(String::from).collect();
        for _ in 0..=rng.next(3) {
            let at = rng.next(parts.len() + 1);
            let piece = ALPHABET[rng.next(ALPHABET.len())].to_string();
            match rng.next(3) {
                0 => parts.insert(at, piece),
                1 if at < parts.len() => {
                    parts.remove(at);
                }
                _ if at < parts.len() => parts[at] = piece,
                _ => parts.push(piece),
            }
        }
        parts.concat()
    }

    #[test]
    fn each_check_accepts_exactly_what_its_pattern_did() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for (label, check, pattern, seeds) in CASES {
            let re = Regex::new(pattern).unwrap();
            let mut inputs: Vec<String> = seeds.iter().map(|s| s.to_string()).collect();
            for _ in 0..4000 {
                let seed = seeds[rng.next(seeds.len())];
                inputs.push(mutate(&mut rng, seed));
                let len = rng.next(12);
                inputs.push(
                    (0..len)
                        .map(|_| ALPHABET[rng.next(ALPHABET.len())])
                        .collect(),
                );
            }
            let mut accepted = 0;
            for input in &inputs {
                assert_eq!(check(input), re.is_match(input), "{label}: {input:?}");
                accepted += usize::from(check(input));
            }
            // Mutation must reach both sides of every check, or the comparison proves little.
            assert!(
                accepted > 0 && accepted < inputs.len(),
                "{label}: {accepted}/{}",
                inputs.len()
            );
        }
    }

    #[test]
    fn uid_refuses_non_ascii_digits() {
        assert!(Regex::new(r"^\d+(:\d+)?$").unwrap().is_match("\u{0661}"));
        assert!(!uid("\u{0661}"));
    }
}
