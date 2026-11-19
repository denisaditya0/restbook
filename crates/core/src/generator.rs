//! Send-time values such as `{{$uuid}}` and `{{$timestamp}}`.
//!
//! A generator is not a variable: it has no stored value and cannot be
//! overridden, so the `$` prefix is reserved and wins over any scope. The clock
//! and random seed are the only non-request inputs, so tests can fix them.

use std::cell::Cell;

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Utc};

/// Values a request can ask for at send time instead of storing.
///
/// Each random draw advances a counter instead of mutating the seed, so `call`
/// takes `&self` and one set of generators can be borrowed by
/// [`crate::template::resolve_with`] for the whole request.
#[derive(Debug, Clone)]
pub struct Generators {
    now: DateTime<Utc>,
    seed: u64,
    draws: Cell<u64>,
}

/// Every generator in the form a user types, in the order the editor lists it.
/// The parameterised timestamp is shown as an example because the popup has no
/// way to offer a blank pattern.
const NAMES: &[&str] = &[
    "$uuid",
    "$uuid7",
    "$timestamp",
    "$timestampMs",
    "$isoTimestamp",
    "$timestamp('%Y-%m-%d')",
    "$randomInt",
    "$randomBoolean",
    "$randomAlphaNumeric",
];

const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

impl Generators {
    /// The real clock and a seed taken from it, for an actual send.
    pub fn now() -> Self {
        let now = Utc::now();
        // The seed only has to differ between runs, and the instant does. Losing
        // the nanoseconds of an out-of-range timestamp is harmless.
        let seed = now.timestamp_nanos_opt().unwrap_or_default() as u64;
        Self::at(now, seed)
    }

    /// A fixed clock and seed, so a test sees the same values every run.
    pub fn at(now: DateTime<Utc>, seed: u64) -> Self {
        Self {
            now,
            seed,
            draws: Cell::new(0),
        }
    }

    /// Resolves the token between the braces, `$` included. `None` means the
    /// name is not a generator; the caller turns that into an error.
    pub fn call(&self, token: &str) -> Option<String> {
        match token {
            "$uuid" => Some(uuid::Uuid::new_v4().to_string()),
            // v7 is timestamp-ordered, so a list of IDs sorts by creation.
            "$uuid7" => Some(uuid::Uuid::now_v7().to_string()),
            "$timestamp" => Some(self.now.timestamp().to_string()),
            "$timestampMs" => Some(self.now.timestamp_millis().to_string()),
            "$isoTimestamp" => Some(self.now.to_rfc3339()),
            "$randomInt" => Some((self.next() % 1001).to_string()),
            "$randomBoolean" => Some(
                if self.next() & 1 == 1 {
                    "true"
                } else {
                    "false"
                }
                .to_owned(),
            ),
            "$randomAlphaNumeric" => {
                let index = (self.next() % ALPHABET.len() as u64) as usize;
                Some((ALPHABET[index] as char).to_string())
            }
            _ => self.custom_timestamp(token),
        }
    }

    /// The generator names, for the editor's completion popup. This list is the
    /// contract with `call`; the tests hold the two together.
    pub fn names() -> &'static [&'static str] {
        NAMES
    }

    /// One short line per generator, for the popup's detail column. Unknown
    /// names have no description.
    pub fn describe(name: &str) -> &'static str {
        match name {
            "$uuid" => "UUID v4",
            "$uuid7" => "timestamp-ordered UUID v7",
            "$timestamp" => "Unix seconds",
            "$timestampMs" => "Unix milliseconds",
            "$isoTimestamp" => "RFC 3339",
            "$timestamp('%Y-%m-%d')" => "custom strftime pattern",
            "$randomInt" => "0–1000",
            "$randomBoolean" => "true or false",
            "$randomAlphaNumeric" => "one alphanumeric character",
            _ => "",
        }
    }

    /// `$timestamp('FMT')`, where `FMT` is a chrono strftime pattern. Anything
    /// that is not exactly one quoted pattern is unknown, not a best effort.
    fn custom_timestamp(&self, token: &str) -> Option<String> {
        let inner = token.strip_prefix("$timestamp(")?.strip_suffix(')')?;
        let format = inner.strip_prefix('\'')?.strip_suffix('\'')?;
        if format.contains('\'') {
            return None;
        }
        // `format` panics on an invalid pattern when displayed, so check it here.
        if StrftimeItems::new(format).any(|item| matches!(item, Item::Error)) {
            return None;
        }
        Some(self.now.format(format).to_string())
    }

    /// One SplitMix64 step, with the seed advanced by the draw counter. This is
    /// deterministic and dependency-free; it is not intended to be unguessable.
    fn next(&self) -> u64 {
        let draw = self.draws.get();
        self.draws.set(draw.wrapping_add(1));
        splitmix64(self.seed.wrapping_add(draw.wrapping_mul(GAMMA)))
    }
}

fn splitmix64(state: u64) -> u64 {
    let mut z = state.wrapping_add(GAMMA);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed() -> Generators {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        Generators::at(now, 42)
    }

    #[test]
    fn fixed_instant_makes_the_clocks_exact() {
        let generators = fixed();
        assert_eq!(generators.call("$timestamp").as_deref(), Some("1700000000"));
        assert_eq!(
            generators.call("$timestampMs").as_deref(),
            Some("1700000000000")
        );
        assert_eq!(
            generators.call("$isoTimestamp").as_deref(),
            Some("2023-11-14T22:13:20+00:00")
        );
    }

    #[test]
    fn timestamp_takes_one_quoted_pattern() {
        let generators = fixed();
        assert_eq!(
            generators.call("$timestamp('%Y-%m-%d')").as_deref(),
            Some("2023-11-14")
        );
        // `%Q` is not a strftime specifier, and the quote is unterminated.
        assert_eq!(generators.call("$timestamp('%Q')"), None);
        assert_eq!(generators.call("$timestamp('%Y-%m-%d)"), None);
    }

    fn draws(generators: &Generators) -> Vec<String> {
        (0..8)
            .map(|_| generators.call("$randomAlphaNumeric").unwrap())
            .collect()
    }

    #[test]
    fn the_same_seed_repeats_and_draws_advance() {
        let first = draws(&fixed());
        let second = draws(&fixed());

        // Two generators built the same way see the same sequence, while one
        // generator's own draws move forward.
        assert_eq!(first, second);
        assert!(first.windows(2).any(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn random_generators_stay_in_range() {
        let generators = fixed();
        for _ in 0..100 {
            let int = generators.call("$randomInt").unwrap();
            assert!(int.parse::<u64>().unwrap() <= 1000, "{int} out of range");

            let boolean = generators.call("$randomBoolean").unwrap();
            assert!(boolean == "true" || boolean == "false");

            let alphanumeric = generators.call("$randomAlphaNumeric").unwrap();
            assert_eq!(alphanumeric.len(), 1);
            assert!(alphanumeric.chars().all(|c| c.is_ascii_alphanumeric()));
        }
    }

    #[test]
    fn uuids_are_hyphenated_and_versioned() {
        let generators = fixed();
        assert_uuid(&generators.call("$uuid").unwrap(), 4);
        assert_uuid(&generators.call("$uuid7").unwrap(), 7);
    }

    #[test]
    fn unknown_tokens_are_none() {
        assert_eq!(fixed().call("$nope"), None);
        assert_eq!(fixed().call("uuid"), None);
    }

    #[test]
    fn names_and_descriptions_cover_every_generator() {
        let generators = fixed();
        for name in Generators::names() {
            assert!(
                generators.call(name).is_some(),
                "{name} is listed but not callable"
            );
            assert!(
                !Generators::describe(name).is_empty(),
                "{name} has no description"
            );
        }
        assert_eq!(Generators::describe("$nope"), "");
    }

    fn assert_uuid(value: &str, version: usize) {
        assert_eq!(value.len(), 36);
        let hyphens: Vec<usize> = value
            .char_indices()
            .filter(|(_, c)| *c == '-')
            .map(|(index, _)| index)
            .collect();
        assert_eq!(hyphens, vec![8, 13, 18, 23]);

        let parsed = uuid::Uuid::parse_str(value).unwrap();
        assert_eq!(parsed.get_version_num(), version);
    }
}
