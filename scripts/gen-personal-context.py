#!/usr/bin/env python3
"""Synthesise a household personal-context corpus in the shape PAI-8 ingests.

The output is deliberately NOT a Rust fixture. Every line of `household.jsonl` is
exactly one `pond_core::context::ingest::RawItem` plus the `source_id` that routes
it -- which is to say, it is the payload schema for the `POST /api/v1/context/ingest`
route that `SourceAvailability::AwaitingIngestRoute` names and that does not exist
yet. Authoring the corpus is how that schema gets designed before the route is written.

Note what is absent: no line carries a `profile_id`. The owner is resolved from the
source, per the ingest invariant enforced by `raw_item_does_not_name_its_own_owner`
in ingest.rs. A corpus that named its own owner would encode the wrong shape.

Deterministic: same seed in, same bytes out. Regenerate with
    python3 scripts/gen-personal-context.py
"""

import hashlib
import json
import random
from datetime import datetime, timedelta, timezone
from pathlib import Path

SEED = 20260904
# Anchored, not "now". The corpus ages otherwise and every recency score in the
# harness drifts with the wall clock. The harness reads this back as its `now`.
CORPUS_NOW = datetime(2026, 9, 4, 18, 30, tzinfo=timezone.utc)
OUT = Path(__file__).resolve().parent.parent / "fixtures" / "personal-context"

rng = random.Random(SEED)

# ---------------------------------------------------------------- sources

SOURCES = [
    # Host-device connectors: the shape a per-OS companion would push.
    ("mac-eventkit-wanjiku", "calendar", "eventkit-macos", "profile_wanjiku",
     ["calendars.read"]),
    ("mac-spotlight-wanjiku", "files", "spotlight-macos", "profile_wanjiku",
     ["files.metadata.read"]),
    ("eds-cal-otieno", "calendar", "eds-gnome", "profile_otieno",
     ["calendars.read"]),
    ("tracker3-files-otieno", "files", "tracker3-gnome", "profile_otieno",
     ["files.metadata.read"]),
    ("winrt-cal-amara", "calendar", "winrt-appointments", "profile_amara",
     ["appointments.read"]),
    # Protocol connectors that already exist and already sync.
    ("imap-wanjiku", "mail", "imap-gmail", "profile_wanjiku", ["mail.read"]),
    ("imap-otieno", "mail", "imap-fastmail", "profile_otieno", ["mail.read"]),
    ("caldav-shared", "calendar", "caldav-google", "profile_wanjiku",
     ["calendars.read"]),
    # Gated kinds. Kept in the corpus on purpose: they are what quantifies the gap.
    ("gotg-otieno", "mobile", "gotg", "profile_otieno", ["location", "contacts"]),
    ("slack-wanjiku", "chat", "slack", "profile_wanjiku", ["channels.history"]),
    # Kinds the pond already holds, which need no connector at all.
    ("hall-pir", "sensor", "pond-sensor", "profile_wanjiku", []),
    ("door-cam", "camera", "pond-camera", "profile_wanjiku", []),
    ("voice-pond", "voice", "pond-voice", "profile_wanjiku", []),
]

items = []          # dicts written to household.jsonl
expectations = []   # dicts written to expectations.jsonl


def at(days_ago, hh, mm=0):
    d = CORPUS_NOW - timedelta(days=days_ago)
    return d.replace(hour=hh, minute=mm, second=0, microsecond=0)


def add(source_id, kind, when, title, body="", participants=None, ext=None):
    """Append one RawItem-shaped line. `ext` only when a query needs to name it."""
    if ext is None:
        base = f"{source_id}|{title}|{when.isoformat()}"
        ext = "gen-" + hashlib.sha1(base.encode()).hexdigest()[:14]
    items.append({
        "source_id": source_id,
        "external_id": ext,
        "kind": kind,
        "occurred_at": when.isoformat().replace("+00:00", "Z"),
        "title": title,
        "body": body,
        "participants": participants or [],
    })
    return ext


def expect(ext, findings, note):
    """What the redactor must find in this item. Kept OUT of the wire schema."""
    expectations.append({"external_id": ext, "expect_findings": findings, "note": note})


# ------------------------------------------------- valid check-digit bait
# Random digits do not exercise the redactor: `is_payment_card` runs Luhn and
# `passes_iban_checksum` runs mod-97, so invalid bait proves only that invalid
# bait is rejected. These are synthesised to actually pass.

def luhn_card(prefix="4539"):
    body = prefix + "".join(str(rng.randint(0, 9)) for _ in range(11))
    total, alt = 0, True
    for ch in reversed(body):
        d = int(ch)
        if alt:
            d *= 2
            if d > 9:
                d -= 9
        total += d
        alt = not alt
    return body + str((10 - total % 10) % 10)


def valid_iban(country="GB", bank="NWBK"):
    acct = "".join(str(rng.randint(0, 9)) for _ in range(14))
    stem = bank + acct
    rearranged = stem + country + "00"
    num = "".join(str(ord(c) - 55) if c.isalpha() else c for c in rearranged)
    check = 98 - (int(num) % 97)
    return f"{country}{check:02d}{stem}"


CARD = luhn_card()
IBAN = valid_iban()
API_KEY = "sk-live-" + "".join(
    rng.choice("abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789")
    for _ in range(34)
)

# ---------------------------------------------------------------- threads
# Hand-authored, because retrieval quality is only measurable against content
# that actually correlates across sources. Volume comes from the recurrences
# and the ambient sources below; meaning comes from here.

# Thread A -- Amara's term 3 school fees. Four sources, one obligation.
FEE_MAIL = add("imap-wanjiku", "message", at(23, 9, 12),
    "St Mary's Girls: Term 3 fee statement",
    "Dear Parent, Term 3 fees of KES 48,500 are due by 12 September. Balance carried "
    "forward from Term 2 is KES 3,200. Pay via paybill 522522, account 1094SMG. For "
    "queries call the bursar on 0722 481 903 or reply to this message.",
    ["bursar@stmarys.ac.ke", "wanjiku.a@gmail.com"], ext="fees-statement-t3")
expect(FEE_MAIL, ["phone", "email"],
       "Kenyan mobile, 10 digits in 3 groups, must trip is_plausible_phone. "
       "Paybill 522522 must NOT: six digits is below the card floor and there is "
       "no rule for a paybill. The `email` finding comes from PARTICIPANTS, not the "
       "body: ContextItem::from_parts redacts title, body and every participant and "
       "unions the findings, so any mail item with an address in participants carries "
       "an email finding whatever its body says.")

add("imap-wanjiku", "message", at(9, 7, 40),
    "Reminder: Term 3 fees outstanding",
    "This is a reminder that the balance of KES 48,500 for Amara Otieno remains "
    "unpaid. Late payment attracts a 2 percent charge after 12 September.",
    ["bursar@stmarys.ac.ke"], ext="fees-reminder-t3")

add("caldav-shared", "event", at(-8, 8, 0),
    "Term 3 school fees deadline",
    "Hard deadline from the bursar's statement. Late charge applies after this.",
    [], ext="fees-deadline-event")

add("mac-eventkit-wanjiku", "task", at(22, 20, 15),
    "Pay Amara's term 3 fees",
    "KES 48,500 plus the 3,200 carried forward. Paybill, not the bank transfer.",
    [], ext="fees-task")

FEE_DOC = add("mac-spotlight-wanjiku", "document", at(24, 16, 5),
    "St Marys fee structure 2026.pdf",
    "~/Documents/School/St Marys fee structure 2026.pdf. Termly breakdown: tuition "
    "38,000, boarding 8,000, activity levy 2,500. Signed by the principal.",
    [], ext="fees-structure-doc")

# Thread B -- the water tank. Mail, chat, a document and a visit.
LANDLORD = add("imap-wanjiku", "message", at(17, 19, 48),
    "Re: water tank overflow, house 4",
    "I have spoken to the plumber and he can come Saturday morning. The overflow "
    "pipe is the landlord's responsibility under clause 7, the tank itself is not. "
    "Reach me on +254 733 118 274 if Saturday does not work.",
    ["landlord.kariuki@yahoo.com"], ext="landlord-tank-reply")
expect(LANDLORD, ["phone", "email"],
       "International +254, 12 digits, inside the 8..15 international band. "
       "`email` from the participant address, as above.")

add("slack-wanjiku", "message", at(16, 12, 30),
    "DM with Njoroge the plumber",
    "Njoroge: I will come Saturday by 9. Bring the receipt for the float valve, the "
    "landlord pays for that one not you.",
    ["njoroge.plumber"], ext="plumber-dm")

add("caldav-shared", "event", at(11, 9, 0),
    "Plumber: water tank overflow",
    "Njoroge, house 4. Float valve and the overflow pipe. Receipt goes to Kariuki.",
    [], ext="plumber-visit-event")

add("mac-spotlight-wanjiku", "document", at(40, 11, 20),
    "Tenancy agreement house 4 signed.pdf",
    "~/Documents/House/Tenancy agreement house 4 signed.pdf. Clause 7 covers "
    "structural plumbing and roof drainage. Renewal 31 March 2027.",
    [], ext="tenancy-doc")

# Thread C -- work: the Mombasa consignment. The busiest thread on purpose.
add("imap-wanjiku", "message", at(19, 8, 22),
    "Consignment MSA-4471 held at Mombasa",
    "The container is held pending a KRA valuation query. Clearing agent says two "
    "to three working days. This puts the Nakuru delivery outside the SLA window.",
    ["ops@coastalfreight.co.ke"], ext="consignment-held")

add("imap-wanjiku", "message", at(14, 15, 6),
    "Re: Consignment MSA-4471 held at Mombasa",
    "Valuation query resolved. Container released this morning, on the road tonight, "
    "Nairobi depot Thursday. Nakuru leg needs rebooking.",
    ["ops@coastalfreight.co.ke"], ext="consignment-released")

BANK_MAIL = add("imap-wanjiku", "message", at(13, 10, 44),
    "Demurrage invoice, consignment MSA-4471",
    f"Invoice 7741 for KES 61,200 demurrage. Settle to {IBAN} or by card. Our old "
    f"card on file {CARD} has been withdrawn, please do not reuse it.",
    ["accounts@coastalfreight.co.ke"], ext="demurrage-invoice")
expect(BANK_MAIL, ["iban", "card", "email"],
       "Both are check-digit valid, and both classify Secret, so at "
       "INGEST_REDACTION_LEVEL (Secrets) both must be REPLACED in the stored body, "
       "not merely reported. `email` from the participant address.")

add("mac-spotlight-wanjiku", "document", at(12, 17, 55),
    "Q3 freight reconciliation.xlsx",
    "~/Documents/Work/Q3 freight reconciliation.xlsx. Demurrage line for MSA-4471 "
    "not yet reconciled. Owner Wanjiku, last edited on the MacBook.",
    [], ext="q3-reconciliation")

add("mac-eventkit-wanjiku", "task", at(13, 18, 2),
    "Rebook the Nakuru leg for MSA-4471",
    "Depot Thursday. Needs a new truck slot and a revised SLA note to the client.",
    [], ext="rebook-nakuru")

# Thread D -- travel, with a change. Tests whether the LATER mail wins.
add("imap-wanjiku", "message", at(21, 13, 15),
    "Your Kenya Airways booking KQ 310 is confirmed",
    "Booking reference 7XQ4LM. Nairobi JKIA to Mombasa MBA, 14 September, departing "
    "07:20, arriving 08:25. One checked bag.",
    ["noreply@kenya-airways.com"], ext="flight-confirmed")

add("imap-wanjiku", "message", at(4, 11, 30),
    "Schedule change: KQ 310 on 14 September",
    "Your flight now departs 09:45 instead of 07:20, arriving 10:50. Booking "
    "reference 7XQ4LM. No action needed to accept the change.",
    ["noreply@kenya-airways.com"], ext="flight-changed")

add("caldav-shared", "event", at(-10, 9, 45),
    "KQ 310 NBO to MBA",
    "Departs 09:45 after the schedule change. Was 07:20 on the original booking.",
    [], ext="flight-event")

add("mac-spotlight-wanjiku", "document", at(3, 12, 10),
    "KQ310 boarding pass 7XQ4LM.pdf",
    "~/Downloads/KQ310 boarding pass 7XQ4LM.pdf",
    [], ext="boarding-pass-doc")

# Thread E -- Otieno's teaching, on the Ubuntu box via EDS and Tracker.
add("imap-otieno", "message", at(15, 16, 40),
    "Form 3 chemistry marking due Friday",
    "HOD needs the Form 3 end-of-term chemistry scripts marked and the mark sheet "
    "uploaded by Friday. 62 scripts.",
    ["hod.science@kibera-sec.ac.ke"], ext="marking-due")

add("eds-cal-otieno", "task", at(15, 17, 5),
    "Mark Form 3 chemistry scripts",
    "62 scripts. Mark sheet upload by Friday.", [], ext="marking-task")

add("tracker3-files-otieno", "document", at(15, 20, 30),
    "form3-chem-marksheet.ods",
    "/home/otieno/Documents/marking/form3-chem-marksheet.ods. 41 of 62 entered.",
    [], ext="marksheet-doc")

# Thread F -- health, the most sensitive thread in the corpus.
add("imap-wanjiku", "message", at(8, 14, 12),
    "Appointment confirmed: Dr Wachira, dental",
    "Your appointment is confirmed for 10 September at 15:30 at Parklands Dental. "
    "Please arrive ten minutes early. To reschedule call 020 3742 118.",
    ["reception@parklandsdental.co.ke"], ext="dentist-mail")

add("caldav-shared", "event", at(-6, 15, 30),
    "Dentist, Dr Wachira",
    "Parklands Dental. Arrive 15:20.", [], ext="dentist-event")

# Thread G -- a genuine developer secret sitting in a file, which is the case
# the ApiKey rule exists for.
ENV_DOC = add("tracker3-files-otieno", "document", at(6, 22, 18),
    ".env in ~/projects/marks-portal",
    f"/home/otieno/projects/marks-portal/.env contains STRIPE_SECRET={API_KEY} and "
    "DATABASE_URL=postgres://localhost:5432/marks",
    [], ext="env-file-doc")
expect(ENV_DOC, ["api-key"],
       "UNREACHABLE TODAY, and that is the finding: a checked-in credential is the "
       "case the ApiKey rule exists for, it lives in a file on a laptop, and Files is "
       "AwaitingReadConnector -- so IngestPipeline refuses this item and the rule has "
       "no reachable caller through context ingest. The harness reports it as blocked "
       "rather than failing. sk- prefixed, 34 char body, so the prefix arm of "
       "is_api_key_shaped matches without the entropy fallback.")

# The same credential class, arriving somewhere the pipeline actually accepts. A
# colleague pasting a key into a mail thread is not a contrived case, and it is the
# only way the Secret-replacement path for ApiKey is exercised before Files lands.
KEY_MAIL = add("imap-otieno", "message", at(5, 15, 22),
    "Re: marks portal deploy, staging key",
    f"Here is the staging key so you can run the importer yourself: {API_KEY}. "
    "Rotate it when you are done, it is the shared staging one not yours.",
    ["dev.mwangi@kibera-sec.ac.ke"], ext="key-in-mail")
expect(KEY_MAIL, ["api-key", "email"],
       "The reachable half of the ApiKey case. Secret class, so it must be REPLACED "
       "in the stored body. `email` from the participant address.")

# ------------------------------------------------- false-positive bait
# The measurement nobody has taken. Every finding raises an item to Sensitive
# and, for the three Secret-class kinds, REWRITES the stored body -- so a false
# positive silently corrupts the corpus the model reads. These items are ordinary
# household prose that a permissive regex would plausibly eat.

FP = [
    ("Room booking: 4532 1234 for the AGM",
     "The estate AGM is in hall 4532, doors 1234 to 1500. Bring the service charge "
     "statement.",
     [], "Digit runs that look card-shaped but fail Luhn."),
    ("School year 2026 2027 calendar published",
     "The 2026 2027 academic year runs 6 January to 20 November. Half terms are "
     "unchanged from 2025 2026.",
     [], "Year pairs are 8 and 9 digit runs in two groups -- inside the phone "
         "regex, and the reason is_plausible_phone has a group and digit-count floor."),
    ("Generator service, 10000 hours",
     "The generator is at 10000 hours and the service interval is 12000. Quote was "
     "34,500 including the filter.",
     [], "Five digit runs; below every floor."),
    ("Meter readings for August",
     "Water meter 00100 to 00147, electricity 88214 to 88603. Nairobi 00100 is the "
     "postcode on the bill.",
     [], "00100 is a real Kenyan postcode and is_uk_postcode cannot match it. "
         "A known and deliberate gap, recorded rather than worked around."),
    ("Router moved to the hall cupboard",
     "The access point is on 192.168.1.50 now and the pond answers on "
     "10.0.19041.1234. Wi-Fi unchanged.",
     [], "The case PHONE_SEPARATORS excludes '.' for. Must find nothing."),
    ("Serial on the inverter plate",
     "Inverter serial 3C-8A-19-EE-04-B7, warranty to March 2028.",
     [], "Five plus groups of exactly two: the MAC-address guard."),
    ("Reconciliation notes",
     "The reconciliation of the consignment demurrage against the freight schedule "
     "is outstanding; documentation from the clearing agent is incomplete.",
     [], "Eight ordinary words of 12 or more letters. Single case and no digit, so "
         "is_high_entropy_secret must reject every one of them."),
    ("Commit that broke the marks portal",
     "Reverted 9f3a2b7c1d4e5f60718293a4b5c6d7e8f9012345 which changed the mark "
     "sheet upload path.",
     [], "40 hex chars: length and case pass, is_hex must reject."),
    ("New pond token rotated",
     "Rotated the internal token. The old one was GIAP-2026-INTERNAL and is dead.",
     [], "Uppercase with digits and hyphens but only 18 chars, under the 32 floor."),
]
for i, (title, body, findings, note) in enumerate(FP):
    src = "imap-wanjiku" if i % 2 == 0 else "imap-otieno"
    ext = add(src, "message", at(rng.randint(2, 34), rng.randint(8, 21),
                                 rng.choice([0, 15, 30, 45])),
              title, body, [], ext=f"fp-bait-{i:02d}")
    expect(ext, findings, note)

# ---------------------------------------------------------- recurrences
# Where the volume comes from, and the reason it is realistic volume: a real
# calendar is mostly the same few events over and over, which is exactly what
# makes a fixed-slice context window useless and retrieval necessary.

def weekdays(span_days, weekday_set):
    for d in range(span_days, -14, -1):
        day = CORPUS_NOW - timedelta(days=d)
        if day.weekday() in weekday_set:
            yield d

for d in weekdays(42, {0, 1, 2, 3, 4}):
    add("mac-eventkit-wanjiku", "event", at(d, 9, 15), "Ops standup",
        "Daily standup, dispatch board review.", ["ops-team"])

for d in weekdays(42, {1, 3}):
    add("winrt-cal-amara", "event", at(d, 16, 30), "Swim practice",
        "Aquatics centre, 16:30 to 18:00. Kit bag and goggles.", [])

for d in weekdays(42, {0, 2, 4}):
    add("eds-cal-otieno", "event", at(d, 11, 0), "Form 3 chemistry, lab 2",
        "Double period. Practical on titration this cycle.", [])

for d in weekdays(42, {6}):
    add("caldav-shared", "event", at(d, 10, 0), "Household planning hour",
        "Shopping list, week ahead, anything outstanding on the house.", [])

# ------------------------------------------------------ ambient sources
# Sensor, camera and voice: already Landed, need no connector, and are the only
# kinds whose min_sensitivity is not already Sensitive (Sensor is Internal), so
# they are the only items where a redaction finding actually MOVES sensitivity.

HALL_HOURS = [0, 1, 2, 5, 6, 7, 8, 12, 18, 19, 20, 21, 22, 23]
for _ in range(58):
    d = rng.randint(0, 41)
    h = rng.choice(HALL_HOURS)
    add("hall-pir", "location", at(d, h, rng.randint(0, 59)),
        "Motion in the hall",
        f"Hall PIR triggered, {'quiet hours' if h < 6 else 'ordinary hours'}.", [])

for _ in range(24):
    d = rng.randint(0, 41)
    h = rng.choice([6, 7, 8, 13, 17, 18, 19, 22, 23, 1])
    who = rng.choice(["a recognised face, Otieno", "a recognised face, Wanjiku",
                      "a recognised face, Amara", "an unrecognised face",
                      "no face, a parcel left at the gate"])
    add("door-cam", "location", at(d, h, rng.randint(0, 59)),
        "Door camera event", f"Front gate: {who}.", [])

VOICE = [
    ("Asked about the generator service", "Wanjiku asked whether the generator "
     "service was booked. It is not; the quote was 34,500 and nobody has replied "
     "to the technician."),
    ("Asked what was outstanding on the house", "Answered with the water tank and "
     "the float valve receipt. Wanjiku said the landlord pays for the valve."),
    ("Asked about the fee deadline", "Answered 12 September from the bursar's "
     "statement. Wanjiku said she would pay by paybill on the Friday."),
    ("Asked to move the standup", "Could not: the pond has no calendar write path "
     "and PAI-8 puts write-back out of scope."),
    ("Asked who was at the gate yesterday", "Two events: a recognised face at "
     "17:52 and a parcel at 13:14."),
]
for i, (title, body) in enumerate(VOICE):
    add("voice-pond", "message", at(rng.randint(1, 30), rng.randint(7, 22), 0),
        title, body, [], ext=f"voice-turn-{i:02d}")

# ----------------------------------------- gated kinds, deliberately populated
# Mobile is AwaitingIngestRoute; Chat is AwaitingReadConnector. IngestPipeline
# REFUSES both. Leaving them in the corpus is the point: the harness counts the
# refusals, which turns "P3 is not done" into a number.

for _ in range(11):
    d = rng.randint(0, 41)
    place = rng.choice(["Kibera Secondary School", "Sarit Centre", "home, house 4",
                        "Nairobi CBD", "Parklands", "the aquatics centre"])
    add("gotg-otieno", "location", at(d, rng.randint(7, 20), rng.randint(0, 59)),
        f"Arrived at {place}", f"Phone reported arrival at {place}.", [])

CHAT = [
    "Estate WhatsApp: the water will be off on Tuesday from 8 to 4.",
    "Njoroge: float valve replaced, receipt photographed and sent to Kariuki.",
    "Ops channel: MSA-4471 cleared customs, on the road tonight.",
    "Estate WhatsApp: AGM moved to hall 4532, same date.",
    "Amara: swim practice cancelled Thursday, pool maintenance.",
    "Ops channel: Nakuru truck slot confirmed for Friday 06:00.",
    "HOD: mark sheet portal is down, upload Monday instead.",
    "Estate WhatsApp: gate motor repaired, code unchanged.",
]
for i, body in enumerate(CHAT):
    add("slack-wanjiku", "message", at(rng.randint(0, 38), rng.randint(8, 21),
                                       rng.choice([0, 20, 40])),
        body.split(":")[0], body, [], ext=f"chat-{i:02d}")

# ------------------------------------------------------------------ filler
# Ordinary traffic. Not padding: without it the labelled queries retrieve out of
# a corpus where every item is already relevant, and recall@k means nothing.

MAIL_FILLER = [
    ("KPLC: your August bill is ready", "Account 4471029. Units 214, amount KES 4,812. Due 15 September."),
    ("Safaricom: data bundle expiring", "Your monthly 20GB bundle expires in three days."),
    ("NHIF contribution received", "Your contribution for August has been received and posted."),
    ("Nairobi Water: meter reading due", "A meter reader will visit house 4 between Monday and Wednesday."),
    ("Your Naivas receipt", "Thank you for shopping. Total KES 6,340. Points balance 812."),
    ("Coastal Freight: weekly dispatch board", "Attached is the dispatch board for the coming week."),
    ("KRA: iTax filing reminder", "Your annual return is due by 30 June. File early to avoid the queue."),
    ("Parents' association newsletter", "Sports day is 26 September. Volunteers needed for the stalls."),
    ("Re: quotation for the generator service", "Revised quote KES 34,500 including the fuel filter and labour."),
    ("Your subscription renews soon", "Your annual plan renews on 20 September at the current rate."),
    ("Kibera Secondary: staff meeting agenda", "Agenda: results analysis, timetable for next term, exam security."),
    ("Insurance: motor policy renewal", "Policy for KCB 441X expires 30 September. Renew online or at any branch."),
    ("Sarit Centre: parking receipt", "Two hours, KES 200. Bay 118."),
    ("Delivery scheduled for tomorrow", "Your order will arrive between 10:00 and 14:00. No signature needed."),
    ("Re: Nakuru SLA note", "Client acknowledged the revised window. No credit note required."),
    ("Bank statement available", "Your statement for August is ready in the app. No action needed."),
    ("Aquatics centre: term fees", "Swim squad fees for the term are KES 7,500, payable by 20 September."),
    ("Your appointment reminder", "This is a reminder of your appointment tomorrow. Reply STOP to opt out."),
    ("Coastal Freight: rate card update", "New rates apply from 1 October. The Mombasa to Nairobi leg is unchanged."),
    ("School bus route change", "From Monday the bus will use the Ngong Road stop, not the Junction."),
]
for i in range(96):
    subj, body = MAIL_FILLER[i % len(MAIL_FILLER)]
    src = "imap-wanjiku" if i % 3 else "imap-otieno"
    suffix = "" if i < len(MAIL_FILLER) else f" (thread {i // len(MAIL_FILLER) + 1})"
    add(src, "message",
        at(rng.randint(0, 41), rng.randint(6, 22), rng.choice([0, 8, 17, 23, 41, 55])),
        subj + suffix, body, [])

FILE_FILLER = [
    ("payslip-august-2026.pdf", "~/Documents/Payslips/payslip-august-2026.pdf"),
    ("dispatch-board-wk36.xlsx", "~/Documents/Work/dispatch-board-wk36.xlsx"),
    ("amara-report-card-term2.pdf", "~/Documents/School/amara-report-card-term2.pdf"),
    ("kplc-bill-august.pdf", "~/Downloads/kplc-bill-august.pdf"),
    ("generator-quote-revised.pdf", "~/Downloads/generator-quote-revised.pdf"),
    ("titration-practical-worksheet.odt", "/home/otieno/Documents/teaching/titration-practical-worksheet.odt"),
    ("form3-scheme-of-work.ods", "/home/otieno/Documents/teaching/form3-scheme-of-work.ods"),
    ("motor-policy-2026.pdf", "~/Documents/Insurance/motor-policy-2026.pdf"),
    ("house4-inventory-photos", "~/Pictures/House/house4-inventory-photos, 34 images"),
    ("itax-return-2025.pdf", "~/Documents/Tax/itax-return-2025.pdf"),
    ("swim-squad-timetable.pdf", "~/Documents/School/swim-squad-timetable.pdf"),
    ("float-valve-receipt.jpg", "~/Pictures/Receipts/float-valve-receipt.jpg"),
    ("nakuru-sla-note.docx", "~/Documents/Work/nakuru-sla-note.docx"),
    ("marks-portal-backup.sql", "/home/otieno/projects/marks-portal/marks-portal-backup.sql"),
    ("agm-minutes-july.pdf", "~/Documents/House/agm-minutes-july.pdf"),
]
for i in range(48):
    name, path = FILE_FILLER[i % len(FILE_FILLER)]
    src = "mac-spotlight-wanjiku" if i % 2 == 0 else "tracker3-files-otieno"
    rev = "" if i < len(FILE_FILLER) else f" (revision {i // len(FILE_FILLER) + 1})"
    add(src, "document",
        at(rng.randint(0, 41), rng.randint(8, 23), rng.choice([5, 12, 33, 48])),
        name + rev, path, [])

EVENT_FILLER = [
    ("Dispatch board review", "Weekly, with the clearing agents."),
    ("Estate AGM", "Hall 4532. Service charge and the gate motor."),
    ("Parents evening", "Form 2, twenty minute slots from 17:00."),
    ("Sports day", "All day. Volunteers on the stalls from 08:00."),
    ("Staff meeting", "Results analysis and exam security."),
    ("Car service, KCB 441X", "Ngong Road branch, drop off at 08:00."),
    ("Client review, Nakuru account", "Revised SLA and the demurrage line."),
    ("Grocery run", "Naivas, the monthly shop."),
    ("Church, morning service", "Family, 09:00."),
    ("Swim gala, regional heats", "All day at the aquatics centre."),
]
for i in range(30):
    title, body = EVENT_FILLER[i % len(EVENT_FILLER)]
    src = rng.choice(["mac-eventkit-wanjiku", "eds-cal-otieno", "caldav-shared",
                      "winrt-cal-amara"])
    add(src, "event",
        at(rng.randint(-12, 41), rng.randint(7, 20), rng.choice([0, 30])),
        title, body, [])

# ------------------------------------------------------------------ queries
# The labelled set. `relevant` names external_ids, so recall@k is computable the
# moment an EmbeddingProvider works on the target hardware -- and computable for
# the keyword fallback today, which is the comparison that matters.

QUERIES = [
    ("when are Amara's school fees due",
     ["fees-statement-t3", "fees-deadline-event", "fees-task", "fees-reminder-t3"]),
    ("how much do we owe the school this term",
     ["fees-statement-t3", "fees-structure-doc", "fees-reminder-t3"]),
    ("how do I pay the school, paybill or bank",
     ["fees-statement-t3", "fees-task"]),
    ("what did the landlord say about the water tank",
     ["landlord-tank-reply", "plumber-dm", "tenancy-doc"]),
    ("when is the plumber coming",
     ["plumber-visit-event", "plumber-dm", "landlord-tank-reply"]),
    ("who pays for the float valve",
     ["landlord-tank-reply", "plumber-dm", "tenancy-doc"]),
    ("where is the tenancy agreement",
     ["tenancy-doc"]),
    ("what is happening with the Mombasa consignment",
     ["consignment-held", "consignment-released", "demurrage-invoice",
      "q3-reconciliation", "rebook-nakuru"]),
    ("has the container been released",
     ["consignment-released", "consignment-held"]),
    ("what do we owe on demurrage",
     ["demurrage-invoice", "q3-reconciliation"]),
    ("what time does my flight leave",
     ["flight-changed", "flight-event", "flight-confirmed"]),
    ("did my flight time change",
     ["flight-changed", "flight-confirmed", "flight-event"]),
    ("what is my booking reference",
     ["flight-confirmed", "flight-changed", "boarding-pass-doc"]),
    ("when is the dentist",
     ["dentist-event", "dentist-mail"]),
    ("what time is Amara's swim practice",
     ["swim-squad-timetable.pdf"]),
    ("what is outstanding on the marking",
     ["marking-due", "marking-task", "marksheet-doc"]),
    ("who do I contact about the exam marking",
     ["marking-due"]),
    ("did we ever book the generator service",
     ["voice-turn-00"]),
    ("what did we decide about the generator",
     ["voice-turn-00"]),
    ("was anyone at the gate yesterday",
     ["voice-turn-04"]),
    ("is there a secret checked into the marks portal",
     ["env-file-doc"]),
    ("when is the estate AGM",
     ["fp-bait-00"]),
    ("what are the meter readings for August",
     ["fp-bait-03"]),
    ("what address is the pond on",
     ["fp-bait-04"]),
    ("what is the inverter serial number",
     ["fp-bait-05"]),
]

# One labelled query needed a stably-named document rather than a filler whose
# id is a content hash.
add("mac-spotlight-wanjiku", "document", at(28, 19, 40),
    "Swim squad timetable term 3.pdf",
    "~/Documents/School/Swim squad timetable term 3.pdf. Squad B trains Tuesday "
    "and Thursday 16:30 to 18:00, gala heats on the last Saturday.",
    [], ext="swim-timetable-doc")
QUERIES = [(q, ["swim-timetable-doc"] if r == ["swim-squad-timetable.pdf"] else r)
           for q, r in QUERIES]

# ------------------------------------------------------------------- write
items.sort(key=lambda it: (it["occurred_at"], it["source_id"], it["external_id"]))

dupes = len(items) - len({(i["source_id"], i["external_id"]) for i in items})
assert dupes == 0, f"{dupes} items collide on (source_id, external_id)"

known = {s[0] for s in SOURCES}
for it in items:
    assert it["source_id"] in known, f"unknown source {it['source_id']}"
    assert "profile_id" not in it, "an item must never name its own owner"

labelled = {e for _, rel in QUERIES for e in rel}
have = {i["external_id"] for i in items}
missing = labelled - have
assert not missing, f"queries name items that do not exist: {sorted(missing)}"

OUT.mkdir(parents=True, exist_ok=True)


def write_jsonl(name, rows):
    with (OUT / name).open("w") as f:
        for r in rows:
            f.write(json.dumps(r, sort_keys=True) + "\n")


write_jsonl("household.jsonl", items)
write_jsonl("expectations.jsonl", expectations)
write_jsonl("queries.jsonl", [{"query": q, "relevant": r} for q, r in QUERIES])

with (OUT / "sources.json").open("w") as f:
    json.dump([
        {"id": i, "kind": k, "provider": p, "profile_id": prof, "scopes": sc}
        for i, k, p, prof, sc in SOURCES
    ], f, indent=2, sort_keys=True)
    f.write("\n")

by_kind = {}
for it in items:
    src = next(s for s in SOURCES if s[0] == it["source_id"])
    by_kind[src[1]] = by_kind.get(src[1], 0) + 1

with (OUT / "manifest.json").open("w") as f:
    json.dump({
        "seed": SEED,
        "corpus_now": CORPUS_NOW.isoformat().replace("+00:00", "Z"),
        "generator": "scripts/gen-personal-context.py",
        "items": len(items),
        "sources": len(SOURCES),
        "queries": len(QUERIES),
        "expectations": len(expectations),
        "items_by_source_kind": dict(sorted(by_kind.items())),
        "span_days": 42,
    }, f, indent=2, sort_keys=True)
    f.write("\n")

print(f"{len(items)} items, {len(SOURCES)} sources, {len(QUERIES)} queries, "
      f"{len(expectations)} expectations")
for k, v in sorted(by_kind.items()):
    print(f"  {k:9} {v}")
