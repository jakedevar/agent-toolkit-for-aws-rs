//! Per-slice pins of the global usage text (#5360).
//!
//! The usage text is printed for every command's `--help` and appended to every
//! usage error, so one slice's synopsis or description edit changes the help
//! bytes of every command (#4936). Any change of it must therefore be
//! deliberate. The text is assembled from the shared frame ([`USAGE_HEAD`],
//! [`USAGE_MIDDLE`], [`USAGE_TAIL`]) and, per slice, its synopsis, description
//! and exit-code notes, placed by two ranks and the slice name. This file pins
//! each of those inputs:
//!
//! - [`SLICE_PINS`]: one line per slice, sorted by slice name like
//!   [`SLICES`]: the name, the two ranks, and the length and FNV-1a hash of the
//!   slice's synopsis, description and exit notes. Two slices that add or edit
//!   their own help touch different lines, so their merge has no textual
//!   conflict and no rebuild is needed to resolve one. Two slices that land in
//!   the same alphabetical gap conflict trivially: keep both lines in order.
//! - [`SHARED_PIN`]: the length and hash of the shared frame.
//! - `the_usage_text_is_the_pinned_fragments_in_pinned_order` assembles the
//!   expected text from the pinned ranks without the registry's `const`
//!   machinery and compares it with [`USAGE`], so a change of the ordering
//!   code fails too.
//!
//! # Re-pin procedure
//!
//! Do not compute hashes by hand and do not run a probe build. When the usage
//! text changes (a new slice, an edited synopsis or description, a changed
//! rank), `every_slice_usage_fragment_is_pinned` fails and prints, per slice,
//! the exact replacement line (or `add`/`delete`); `the_shared_usage_frame_is_pinned`
//! prints the replacement `SHARED_PIN` line. Review that the usage change is
//! the one you intended, paste the printed lines into [`SLICE_PINS`] at the
//! printed place (the table stays sorted by name), and say in the Issue that
//! the usage text changed deliberately. After a merge of rolling, a red run
//! prints the lines of both sides: paste what it prints, never merge the
//! numbers.

use super::{Slice, SLICES, USAGE, USAGE_HEAD, USAGE_MIDDLE, USAGE_TAIL};

/// A slice's pin: name, synopsis rank, description rank, then the combined
/// length and the FNV-1a hash of synopsis, description and exit notes.
type Pin = (&'static str, u16, u16, usize, u64);

/// The length and hash of the shared frame (head, middle and tail).
const SHARED_PIN: (usize, u64) = (2172, 0xef10_870b_c95f_9348);

/// One line per slice, ascending by name. See the module docs for how to edit.
#[rustfmt::skip]
const SLICE_PINS: &[Pin] = &[
    ("agents-build", 1000, 1000, 592, 0x4475_7d51_e7ec_2dcb),
    ("agents-pay", 1000, 1000, 1169, 0x8b86_fec8_c506_878d),
    ("agents-pay-admin", 1000, 1000, 1562, 0xdff6_6e77_9cee_0b4e),
    ("ai-data", 250, 250, 1158, 0x5f1c_538d_8b18_bb70),
    ("ai-models", 260, 260, 1388, 0x01bd_19e3_0944_b2da),
    ("ai-payloads", 1000, 1000, 877, 0xe29c_45f1_2039_1183),
    ("asm-exec", 340, 330, 755, 0x8f0d_9c6a_280d_05e8),
    ("aurora", 240, 240, 1361, 0x7372_79c7_a841_5524),
    ("bedrock-agent", 270, 270, 821, 0x87ff_1b3d_fda5_19cc),
    ("bedrock-probes", 1000, 1000, 1694, 0x02cc_9c51_035c_952b),
    ("bedrock-report", 280, 280, 1251, 0x3124_6e21_5242_c476),
    ("bedrock-resolve", 320, 320, 1639, 0x1a97_5e17_bd8a_74bf),
    ("cloudwatch-api", 300, 300, 2276, 0xc323_55fa_f75f_4211),
    ("cloudwatch-domain", 290, 290, 1120, 0x9d27_7127_6d15_3fe8),
    ("docgraph", 1000, 1000, 1806, 0x8c35_d6f6_0c6a_b109),
    ("dynamodb", 220, 220, 610, 0x706f_2bf7_8c4e_bad8),
    ("dynamodb-bench", 1000, 1000, 732, 0x8f36_c2e7_5cfe_58fe),
    ("dynamodb-deploy", 230, 230, 725, 0x820a_1ac1_e8c8_dd42),
    ("elasticache", 310, 310, 2271, 0xfe79_aead_a0d1_1d27),
    ("elasticache-tunnel", 1000, 1000, 1076, 0x06b3_f292_c296_052f),
    ("launch", 1000, 1000, 1674, 0x9280_3dfc_f446_fd72),
    ("manifests", 20, 20, 448, 0x9d8f_fd4f_d75e_ebca),
    ("marketplace", 1000, 1000, 1305, 0xef03_2fb4_2159_2eba),
    ("mcp-proxy", 1000, 1000, 1247, 0xbf74_da5f_8a12_705b),
    ("msk", 210, 210, 2058, 0x62ba_d77b_6e06_99e5),
    ("relational", 330, 340, 2231, 0x60dc_8d7b_769d_b953),
    ("skills", 10, 10, 242, 0x1eed_9f2b_572a_8087),
    ("spec", 30, 30, 482, 0x68b6_d419_c896_2de7),
    ("startup-artifacts", 70, 120, 936, 0x2311_9a04_cd0e_52cd),
    ("startup-consent", 90, 80, 444, 0x5799_42ff_803f_b8c7),
    ("startup-diagram", 150, 150, 686, 0x3f20_6270_3324_8e86),
    ("startup-fixtures", 190, 190, 940, 0x5ed1_37b1_26ff_86c4),
    ("startup-model-ids", 60, 60, 309, 0xc9cf_5c2c_b809_bd6f),
    ("startup-model-path", 160, 160, 643, 0x9b17_19ec_1740_9ea8),
    ("startup-model-staleness", 180, 180, 616, 0xf4db_214c_e83a_4b05),
    ("startup-models", 140, 140, 908, 0xdaf5_f766_13b5_8c69),
    ("startup-parity", 100, 90, 558, 0x4eb8_0f59_eb9a_8ba8),
    ("startup-plan", 80, 70, 642, 0x8479_2d65_8939_ce2c),
    ("startup-pricing", 40, 40, 366, 0x2ba4_b127_9ac2_34b2),
    ("startup-program", 50, 50, 464, 0x702a_7726_3d6f_2fd4),
    ("startup-report", 120, 110, 1681, 0x9c11_c725_ffdf_d58d),
    ("startup-scoring", 130, 130, 848, 0xbdc1_8411_3917_42bd),
    ("startup-skill-refs", 170, 170, 996, 0xdaf0_59cf_d197_ec3f),
    ("startup-terraform", 110, 100, 451, 0x5747_374f_5214_571a),
    ("sync", 200, 200, 712, 0xd3b2_d391_4257_9326),
    ("telemetry", 1000, 1000, 1210, 0x575a_ea77_5f67_7dde),
];

/// 64-bit FNV-1a over the parts, each followed by a NUL byte so that moving
/// text between two parts changes the hash.
fn fnv1a64(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// A hash in the source spelling: `0xf788_a966_bf88_eca8`.
fn hex(hash: u64) -> String {
    let digits = format!("{hash:016x}");
    let groups: Vec<&str> = (0..4).map(|i| &digits[i * 4..i * 4 + 4]).collect();
    format!("0x{}", groups.join("_"))
}

fn pin_of(slice: &Slice) -> Pin {
    let parts = [slice.synopsis, slice.description, slice.exit_notes];
    let len = parts.iter().map(|part| part.len()).sum();
    (
        slice.name,
        slice.synopsis_rank,
        slice.description_rank,
        len,
        fnv1a64(&parts),
    )
}

/// The table line of a pin, exactly as it is written in [`SLICE_PINS`].
fn pin_line(pin: &Pin) -> String {
    let (name, synopsis_rank, description_rank, len, hash) = *pin;
    format!(
        "    ({name:?}, {synopsis_rank}, {description_rank}, {len}, {}),",
        hex(hash)
    )
}

fn shared_pin_of(head: &str, middle: &str, tail: &str) -> (usize, u64) {
    (
        head.len() + middle.len() + tail.len(),
        fnv1a64(&[head, middle, tail]),
    )
}

fn shared_line(pin: (usize, u64)) -> String {
    format!(
        "const SHARED_PIN: (usize, u64) = ({}, {});",
        pin.0,
        hex(pin.1)
    )
}

/// Everything wrong between the slices and a pin table, each entry with the
/// line to paste.
fn problems(slices: &[Slice], pins: &[Pin]) -> Vec<String> {
    let mut problems = Vec::new();
    if let Some(pair) = pins.windows(2).find(|pair| pair[0].0 >= pair[1].0) {
        problems.push(format!(
            "SLICE_PINS must be strictly ascending by slice name: {:?} is followed by {:?}; \
             keep one line per slice in sorted order",
            pair[0].0, pair[1].0
        ));
    }
    for slice in slices {
        let now = pin_of(slice);
        match pins.iter().find(|pin| pin.0 == slice.name) {
            None => problems.push(format!(
                "slice {:?} has no usage pin; add this line to SLICE_PINS at its sorted place:\n{}",
                slice.name,
                pin_line(&now)
            )),
            Some(pinned) if *pinned != now => problems.push(format!(
                "the usage text of slice {:?} changed (ranks {}/{}, {} bytes, hash {} were \
                 pinned); if that is deliberate, replace its line in SLICE_PINS with:\n{}",
                slice.name,
                pinned.1,
                pinned.2,
                pinned.3,
                hex(pinned.4),
                pin_line(&now)
            )),
            Some(_) => {}
        }
    }
    for pin in pins {
        if !slices.iter().any(|slice| slice.name == pin.0) {
            problems.push(format!(
                "SLICE_PINS pins slice {:?}, which is not registered; delete this line:\n{}",
                pin.0,
                pin_line(pin)
            ));
        }
    }
    problems
}

/// The usage text built from the pins' ranks, the slices' fragments and the
/// shared frame in plain `Vec` code: the registry's `const` assembly is not
/// used, so a bug in it shows here. A slice without a pin falls back to its own
/// ranks (`every_slice_usage_fragment_is_pinned` reports it).
fn assemble(slices: &[Slice], pins: &[Pin]) -> String {
    let ranks = |slice: &Slice| {
        pins.iter()
            .find(|pin| pin.0 == slice.name)
            .map_or((slice.synopsis_rank, slice.description_rank), |pin| {
                (pin.1, pin.2)
            })
    };
    let mut by_synopsis: Vec<&Slice> = slices.iter().collect();
    by_synopsis.sort_by_key(|slice| (ranks(slice).0, slice.name));
    let mut by_description: Vec<&Slice> = slices.iter().collect();
    by_description.sort_by_key(|slice| (ranks(slice).1, slice.name));

    let mut text = String::from(USAGE_HEAD);
    for slice in by_synopsis {
        text.push_str(slice.synopsis);
    }
    text.push_str(USAGE_MIDDLE);
    for slice in &by_description {
        text.push_str(slice.description);
    }
    text.push_str(USAGE_TAIL);
    for slice in &by_description {
        text.push_str(slice.exit_notes);
    }
    // The usage text has no final newline.
    text.pop();
    text
}

#[test]
fn every_slice_usage_fragment_is_pinned() {
    let problems = problems(SLICES, SLICE_PINS);
    assert!(
        problems.is_empty(),
        "the usage text changed. A deliberate change is re-pinned by pasting the printed \
         line(s) into SLICE_PINS (crates/toolkit-cli/src/registry/usage_pins.rs, see its \
         header); an incidental one must be reverted.\n\n{}",
        problems.join("\n\n")
    );
}

#[test]
fn the_shared_usage_frame_is_pinned() {
    let now = shared_pin_of(USAGE_HEAD, USAGE_MIDDLE, USAGE_TAIL);
    assert_eq!(
        now,
        SHARED_PIN,
        "the shared usage frame (options and exit codes) changed; if that is deliberate, \
         replace the SHARED_PIN line in crates/toolkit-cli/src/registry/usage_pins.rs with:\n{}",
        shared_line(now)
    );
}

#[test]
fn the_usage_text_is_the_pinned_fragments_in_pinned_order() {
    let expected = assemble(SLICES, SLICE_PINS);
    if expected != USAGE {
        let at = expected
            .bytes()
            .zip(USAGE.bytes())
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| expected.len().min(USAGE.len()));
        let context = |text: &str| {
            let start = at.saturating_sub(40);
            let end = (at + 40).min(text.len());
            text.get(start..end).unwrap_or("").to_string()
        };
        panic!(
            "USAGE is not the pinned fragments in the pinned order: lengths {} (expected) and \
             {}; first difference at byte {at}:\nexpected: {:?}\nactual:   {:?}",
            expected.len(),
            USAGE.len(),
            context(&expected),
            context(USAGE)
        );
    }
}

/// A copy of a slice with another synopsis, as a deliberate or incidental edit
/// of its help would produce.
fn with_synopsis(slice: &Slice, synopsis: String) -> Slice {
    Slice {
        synopsis: synopsis.leak(),
        ..*slice
    }
}

#[test]
fn a_mutated_synopsis_fails_with_a_paste_ready_line() {
    let original = SLICES[0];
    let pins: Vec<Pin> = SLICES.iter().map(pin_of).collect();
    assert!(problems(SLICES, &pins).is_empty());

    let mutated = with_synopsis(
        &original,
        format!("{}  toolkit {} extra\n", original.synopsis, original.name),
    );
    let mut slices = SLICES.to_vec();
    slices[0] = mutated;
    let found = problems(&slices, &pins);
    assert_eq!(found.len(), 1, "{found:?}");
    let line = pin_line(&pin_of(&mutated));
    assert!(found[0].contains(&line), "{}", found[0]);
    assert!(
        found[0].contains(&format!("{:?}", original.name)),
        "{}",
        found[0]
    );
    assert_ne!(line, pin_line(&pin_of(&original)));

    // Pasting the printed line over the old one is the whole re-pin.
    let mut repinned = pins.clone();
    repinned[0] = pin_of(&mutated);
    assert!(problems(&slices, &repinned).is_empty());
}

#[test]
fn a_changed_rank_a_missing_pin_and_a_stale_pin_are_reported() {
    let pins: Vec<Pin> = SLICES.iter().map(pin_of).collect();

    let mut slices = SLICES.to_vec();
    slices[1].description_rank += 1;
    let found = problems(&slices, &pins);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains(&pin_line(&pin_of(&slices[1]))),
        "{found:?}"
    );

    let without_first: Vec<Pin> = pins[1..].to_vec();
    let found = problems(SLICES, &without_first);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("add this line"), "{found:?}");
    assert!(found[0].contains(&pin_line(&pins[0])), "{found:?}");

    let found = problems(&SLICES[1..], &pins);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("delete this line"), "{found:?}");

    let mut unsorted = pins.clone();
    unsorted.swap(0, 1);
    let found = problems(SLICES, &unsorted);
    assert!(
        found
            .iter()
            .any(|problem| problem.contains("strictly ascending")),
        "{found:?}"
    );
}

#[test]
fn the_assembly_check_notices_a_swapped_rank() {
    let mut pins: Vec<Pin> = SLICES.iter().map(pin_of).collect();
    let first = 0;
    // Move the first slice behind every other synopsis.
    pins[first].1 = pins.iter().map(|pin| pin.1).max().unwrap_or(0) + 1;
    assert_ne!(assemble(SLICES, &pins), USAGE);
}

#[test]
fn the_pin_line_spelling_is_stable() {
    assert_eq!(hex(0xf788_a966_bf88_eca8), "0xf788_a966_bf88_eca8");
    assert_eq!(hex(0x1), "0x0000_0000_0000_0001");
    assert_eq!(
        pin_line(&("x", 1000, 1000, 12, 0xabcd)),
        "    (\"x\", 1000, 1000, 12, 0x0000_0000_0000_abcd),"
    );
}
