# TTS Text Normalization

The voice pipeline normalizes symbols, units, and abbreviations to their spoken equivalents
before sending text to the Piper TTS engine. This ensures the TTS pronounces "5km" as
"5 kilometers" rather than spelling out "k m".

---

## Implementation

The normalization runs in `normalize_for_speech()` which exists in two identical copies:

- **CLI:** `crates/pond-core/src/shared/services/chat.rs`
- **Desktop:** `pond-desktop/src-tauri/src/tts_text.rs`

Both must be kept in sync. The desktop can't import pond-core (separate Cargo workspace).

---

## Processing Order

The function walks the text character by character. The check order matters:

1. **Time** — `3:45pm` consumed as a unit, not split
2. **Unit suffix** — `5kg` matched after the digit ends
3. **Abbreviations** — `e.g.` expanded at word boundary before dot handling
4. **Period/dot** — context-dependent: "point", "dot", or punctuation
5. **Degree symbol** — `°C`, `°F`, `°K`, bare `°`
6. **Percent** — `%`
7. **Currency** — 19 currency symbols with singular/plural
8. **Ampersand** — `&` at word boundary
9. **At-sign** — `@` at word boundary
10. **Number sign** — `#5` to "number 5"
11. **En dash** — `3–5` to "3 to 5"
12. **Standalone symbols** — math, fractions, legal marks
13. **Fallthrough** — character passed as-is

---

## Unit Suffixes (~85 entries)

Matched only when preceded by a digit. Allows one optional space (`5kg` and `5 kg`).
Sorted longest-first for greedy matching. Alphabetic continuation guard prevents false
positives ("5mining" does not match "min").

| Category | Units | Example |
|----------|-------|---------|
| Compound | km/h, m/s, ft/s, MB/s, fl oz | `100km/h` -> "100 kilometers per hour" |
| Data | KB, MB, GB, TB, PB, EB, KiB, MiB, GiB, TiB | `10GB` -> "10 gigabytes" |
| Data speed | kbps, Mbps, Gbps | `100Mbps` -> "100 megabits per second" |
| Frequency | Hz, kHz, MHz, GHz, THz | `3.5GHz` -> "3.5 gigahertz" |
| Power | W, mW, kW, MW, GW | `500W` -> "500 watts" |
| Voltage | V, mV, kV | `5V` -> "5 volts" |
| Current | A, mA, uA | `10mA` -> "10 milliamps" |
| Resistance | ohm, kohm, Mohm | `10kohm` -> "10 kilohms" |
| Pressure | Pa, kPa, MPa, psi, bar, atm, mmHg | `32psi` -> "32 P S I" |
| Energy | J, kJ, MJ, kWh, kcal, cal, BTU, eV, Wh | `2kWh` -> "2 kilowatt hours" |
| Sound | dB, dBA | `80dB` -> "80 decibels" |
| Duration | ms, ns, us, sec, min, hr, hrs | `200ms` -> "200 milliseconds" |
| Speed | mph, knots | `60mph` -> "60 miles per hour" |
| Length | m, km, cm, mm, nm, um, mi, ft, yd | `5km` -> "5 kilometers" |
| Weight | g, kg, mg, ug, oz, lb, lbs, st | `75kg` -> "75 kilograms" |
| Volume | L, mL, dL, kL, gal, qt, pt | `500mL` -> "500 milliliters" |
| Area | m2, km2, ft2, in2, cm2, ha | `50m2` -> "50 square meters" |
| Volume (cubic) | cm3, m3 | `10m3` -> "10 cubic meters" |

### Decimal + Unit

When a decimal number precedes a unit, the dot is kept as a decimal:
`3.5GHz` -> "3.5 gigahertz" (not "3 point 5 gigahertz").

The function peeks ahead after the decimal digits to check for a unit suffix.

---

## Currency Symbols (19 entries)

| Symbol | Singular | Plural | Example |
|--------|----------|--------|---------|
| $ | dollar | dollars | `$50` -> "50 dollars" |
| GBP | pound | pounds | `GBP30` -> "30 pounds" |
| EUR | euro | euros | `EUR20` -> "20 euros" |
| JPY | yen | yen | `JPY500` -> "500 yen" |
| INR | rupee | rupees | `INR1` -> "1 rupee" |
| RUB | ruble | rubles | |
| KRW | won | won | |
| ILS | shekel | shekels | |
| NGN | naira | naira | |
| PHP | peso | pesos | |
| TRY | lira | lira | |
| UAH | hryvnia | hryvnias | |
| GHS | cedi | cedis | |
| CRC | colon | colones | |
| VND | dong | dong | |
| LAK | kip | kip | |
| MNT | tugrik | tugriks | |
| ESP | peseta | pesetas | |
| CHF | franc | francs | |

Singular detection: `$1` and `$1.00` use singular form; all other values use plural.

---

## Period / Dot Handling

Context-dependent:

| Context | Example | Output |
|---------|---------|--------|
| Between digits (no unit) | `3.14` | "3 point 14" |
| Between digits (with unit) | `3.5GHz` | "3.5 gigahertz" |
| Between letters (domain) | `google.com` | "google dot com" |
| IP address | `192.168.1.1` | "192 point 168 point 1 point 1" |
| Sentence end | `Hello.` | "Hello." (passthrough) |
| Abbreviation | `e.g.` | "for example" |

---

## Abbreviations (35+ entries)

Matched case-insensitively at word boundaries only.

| Category | Abbreviations |
|----------|--------------|
| Latin | e.g., i.e., etc., vs. |
| Titles | Dr., Mr., Mrs., Ms., Prof., Jr., Sr. |
| Business | Inc., Corp., Ltd. |
| Geography | St., Ave., Blvd., Ft., Mt. |
| Reference | No., Vol., Ch., Pg., Fig. |
| Measurement | Approx., Max., Min., Temp., Est. |
| Months | Jan., Feb., Mar., Apr., Jun., Jul., Aug., Sep., Oct., Nov., Dec. |

---

## Standalone Symbols (~35 entries)

| Category | Symbols | Spoken Form |
|----------|---------|-------------|
| Math | +/-, x, /, inf, approx, <=, >=, !=, sqrt, pi | "plus or minus", "times", etc. |
| Superscript | 2, 3 | "squared", "cubed" |
| Fractions | 1/2, 1/3, 2/3, 1/4, 3/4, 1/5-4/5, 1/6, 5/6, 1/8-7/8 | "one half", "one third", etc. |
| Legal | (c), (R), TM, section, paragraph | "copyright", "registered", etc. |
| Typographic | bullet, em dash | comma pause |
| En dash | 3-5 | "3 to 5" (range) or ", " (other) |
| Number sign | #5 | "number 5" |

---

## False Positive Prevention

- **Alphabetic continuation guard:** "5mining" does NOT match "min" because 'i' follows
- **Word boundary check:** abbreviations only fire at word starts (after space/punctuation)
- **Digit prerequisite:** unit suffixes only fire when preceded by a digit
- **Single space tolerance:** "5 kg" matches, "5  kg" (double space) does not

---

## Adding New Entries

1. Add to the appropriate `const` table in BOTH files:
   - `crates/pond-core/src/shared/services/chat.rs`
   - `pond-desktop/src-tauri/src/tts_text.rs`
2. For `UNIT_SUFFIXES`: sort by length descending (longest first)
3. Add a test in `chat.rs` (the `#[cfg(test)]` module)
4. Run `cargo test -p pond-core -- normalize_` to verify
5. Build desktop: `cd pond-desktop/src-tauri && cargo build`

---

## Test Coverage

48 normalization tests covering all categories:
- Temperature, percent, time (existing)
- Unit suffixes: kg, km/h, GHz, kWh, MB, m2, dB, mph (new)
- Currencies: yen, rupee singular/plural (new)
- Standalone: +/-, pi, fractions, copyright, squared, number sign (new)
- Dots: decimal, domain, sentence-end, IP, version (new)
- Abbreviations: e.g., i.e., etc., Dr. (new)
- False positives: "asking", "5mining" (new)
