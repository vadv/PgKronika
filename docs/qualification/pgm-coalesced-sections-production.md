# PGM coalesced-section production measurement

This measurement compares the production `seal` and reader paths at:

- base `d50d9795fbd6a29965224b8c510139d285fe1b02`;
- candidate `18948c58593120f5d1c79d76e23c05461d93ba11`.

Both builds use the same Rust source for the measurement example, frozen at
`2bd9cae706680f89a604ea39132128a69d51bf6b`, and consume the same retained
`active.parts`. The machine-readable result is
[`pgm-coalesced-sections-production-v1.json`](pgm-coalesced-sections-production-v1.json).

## Input and identity

The example creates four journal parts. Each part contains eight rows for every
one of the 76 registered data types and both dictionary sections.

| Property | Value |
| --- | ---: |
| `active.parts` | 1,728,441 bytes |
| Journal SHA-256 | `fbf0e41167e4fccbb579b55bd2ba88f76989f143af4e46d63c93963e26edb9f7` |
| Physical input sections | 312 |
| Data rows | 2,432 |
| Unique dictionary entries | 33 |
| Canonical stream length | 369,513 bytes |
| Logical SHA-256 | `94cf781e2731600cd695267afbd2ce2b8eecc118d9bd718fbc313c9488381833` |

Independent base and candidate `prepare` processes produced byte-identical
journals. The base PGM, candidate PGM, and prepared input have the same
canonical logical digest. The digest includes row multiplicity, NULL markers,
`List<i32>` values, normalized dictionary records, and raw `f32`/`f64` bits.
The canonical stream length is the number of bytes fed to the digest, not a
separate manifest file.

## Physical result

| Property | Base | Candidate |
| --- | ---: | ---: |
| PGM bytes | 1,728,221 | 284,298 |
| Physical sections | 312 | 78 |
| Catalog data rows | 2,432 | 2,432 |
| Catalog dictionary rows | 36 | 33 |
| Logical dictionary entries | 33 | 33 |

The candidate removes 1,443,923 bytes: a 6.078906640215548x reduction, or
83.54967333460246%. PGM SHA-256 values:

- base: `37b3d06a969d99bb695b2e5f4c1dd6b22c9609f3f1d6145a204b5161683ef7b5`;
- candidate: `583d00d84274c71c9bf1591a0ac8faedaebd0abcd66310db4203c48944cea969`.

## Timing, RSS, and I/O

These are single warm-cache observations on a shared Fedora 41 host. They are
not percentiles or service objectives.

| Phase | Base | Candidate |
| --- | ---: | ---: |
| Seal internal wall | 3,565,995 ns | 1,517,596,657 ns |
| Seal max RSS | 2,816 KiB | 11,172 KiB |
| Full production decode | 353,280,412 ns | 53,605,116 ns |
| Read-process max RSS | 5,880 KiB | 6,188 KiB |
| `LocalDirSnapshot` restart | 84,438 ns | 61,565 ns |
| `pg_stat_activity` query | 16,459,636 ns | 3,310,688 ns |
| Query `pread64` calls | 26 | 11 |
| Query `pread64` return bytes | 94,177 | 19,317 |
| Query-process max RSS | 5,804 KiB | 5,832 KiB |

Both restart snapshots contain one sealed unit, no warnings, and no damage.
Both queries return the same 96 rows, two expected fixture gaps, and no
continuation cursor. The candidate query is below the established synthetic
ceiling of 16 `pread64` calls and 150 KiB.

GNU `time` filesystem counters were zero for query input on the warm page
cache. The report retains those counters without treating them as bytes.
`strace` return values provide the logical requested-byte counts above.

## Reproduction

Build the example in release mode for both revisions after copying only the
measurement source and its `kronika-reader` development dependency into the
base source tree:

```sh
cargo +1.96.0 build --release --target x86_64-unknown-linux-musl \
  -p kronika-writer --example measure_coalesced_sections
```

Create and compare the deterministic inputs, then use one of them for both
seals:

```sh
BASE_BIN prepare BASE_INPUT
CANDIDATE_BIN prepare CANDIDATE_INPUT
cmp BASE_INPUT/active.parts CANDIDATE_INPUT/active.parts

BASE_BIN seal BASE_INPUT/active.parts BASE_OUT baseline
CANDIDATE_BIN seal BASE_INPUT/active.parts CANDIDATE_OUT candidate

BASE_BIN read BASE_OUT/segment.pgm baseline
CANDIDATE_BIN read CANDIDATE_OUT/segment.pgm candidate
BASE_BIN query BASE_OUT/segment.pgm
CANDIDATE_BIN query CANDIDATE_OUT/segment.pgm
```

Run each prebuilt phase under GNU `time -v`. For query I/O, trace only
`pread64` and sum successful return values. Validate the preserved report:

```sh
python3 -B scripts/validate-pgm-coalesced-sections.py
```

## Scope

This synthetic measurement covers all 76 contracts registered at the compared
revisions. It does not replace the frozen PR #124 prototype corpus, relabel its
75-contract results, or qualify its natural-segment and 1 GiB journal
thresholds. It also does not qualify real-process PostgreSQL 15–18 restart or
ext4/XFS fault behavior; those remain BDD and final qualification gates.
