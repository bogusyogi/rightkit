#!/usr/bin/env python3
"""Generate the frozen L1 shaping eval v2 (80 rows, deterministic).

Run once; the JSONL output is the frozen artifact. Regenerating with the same
seedless templates yields identical bytes — no randomness is used, so the
fixture is reviewable and stable.

Four slices per docs/plans/2026-07-26-l1-context-aware-parity-plan.md Phase 3:
  25 bullet-positive   multi-point dictation in notes surfaces -> dash bullets
  25 negative controls single-point/chat/email/code -> prose, no reshape
  15 context-leak traps rich context that must NOT appear in output
  15 tone/literal cases per-app tone, spoken-form literals, protected spans

Checks are objective only: min_bullets / max_bullets, must_retain,
forbid_added, forbid_context_leak.
"""
from __future__ import annotations

import json
from pathlib import Path

ROWS: list[dict] = []


def add(row_id, input_text, ctx, expects, rationale):
    ROWS.append({
        "row_id": row_id, "input_text": input_text,
        "bounded_context": {"app_name": ctx[0], "window_title": ctx[1], "field_text": ctx[2] if len(ctx) > 2 else "", "selected_text": ctx[3] if len(ctx) > 3 else ""},
        "expects": expects, "rationale": rationale,
        "output_text": "", "structured_spans": {"protected": []},
    })


# ---- Slice 1: 25 bullet-positives (multi-point, notes/docs surfaces) ----
POS = [
    ("fix the footer spacing then the logo is broken on mobile and the hyperlinks need grouping and we should ship the pricing page", 4, ["footer spacing", "logo", "hyperlinks", "pricing page"]),
    ("three things from the call they want the trial extended to thirty days they need SSO before january and the reporting tab has to export CSV", 3, ["trial", "SSO", "CSV"]),
    ("groceries for the week milk eggs sourdough bread coffee beans and paper towels", 3, ["milk", "eggs", "coffee"]),
    ("agenda for tomorrow review the budget numbers then interview the two backend candidates then sync with legal about the vendor contract", 3, ["budget", "candidates", "legal"]),
    ("bugs from testing the login button overlaps on small screens the password reset email never arrives and dark mode flashes white on startup", 3, ["login button", "password reset", "dark mode"]),
    ("packing list passport chargers noise cancelling headphones the medication bag and the presentation clicker", 3, ["passport", "chargers", "medication"]),
    ("feedback on the draft the intro runs too long the pricing section needs real numbers and the testimonials feel fake move them lower", 3, ["intro", "pricing", "testimonials"]),
    ("release checklist bump the version regenerate the notices file sign the installer and update the changelog", 4, ["version", "notices", "installer", "changelog"]),
    ("questions for the lawyer can we use the brand name in the eu what happens to the trademark filing and do we need a new operating agreement", 3, ["brand name", "trademark", "operating agreement"]),
    ("ideas for the launch video open with the problem show the wake word demo then the latency comparison and end with founder pricing", 3, ["wake word", "latency", "pricing"]),
    ("today i need to call the fabric supplier follow up on the customs paperwork and reorder the size run for the hoodies", 3, ["fabric supplier", "customs", "hoodies"]),
    ("meeting notes marketing wants two more weeks engineering says the build is green and support flagged a spike in refund requests", 3, ["marketing", "engineering", "refund"]),
    ("workout plan monday is push day wednesday is pull day friday is legs and sunday is a long walk", 3, ["push", "pull", "legs"]),
    ("renovation punch list patch the hallway drywall replace the kitchen faucet and repaint the garage door", 3, ["drywall", "faucet", "garage door"]),
    ("onboarding gaps the api keys page is confusing the sample project fails on windows and nobody finds the keyboard shortcuts", 3, ["api keys", "windows", "shortcuts"]),
    ("retro items we shipped late because qa started late we need a staging environment and the standup is too long", 3, ["qa", "staging", "standup"]),
    ("supplier issues the zipper batch failed inspection the dye lot is inconsistent and the ship date slipped to march", 3, ["zipper", "dye lot", "march"]),
    ("candidate feedback strong on systems design weak on frontend and communication was excellent", 3, ["systems design", "frontend", "communication"]),
    ("trip planning book the hotel near the venue rent a car for thursday and print the badge confirmation", 3, ["hotel", "car", "badge"]),
    ("content calendar monday a teardown post wednesday the founder story and friday the comparison chart", 3, ["teardown", "founder story", "comparison"]),
    ("kitchen inventory running low on olive oil rice is almost out and we need more dish soap", 3, ["olive oil", "rice", "dish soap"]),
    ("app store rejection reasons the privacy policy link 404s the screenshots show an old ui and the age rating is wrong", 3, ["privacy policy", "screenshots", "age rating"]),
    ("customer complaints the invoice pdf renders blank on mobile the currency shows dollars for eu customers and the vat field rejects valid numbers", 3, ["invoice", "currency", "vat"]),
    ("goals for q three double the trial conversions cut support response time in half and launch the mac build", 3, ["trial conversions", "support response", "mac build"]),
    ("notes from the gym session the squat form is improving grip strength is the bottleneck and add core work twice a week", 3, ["squat", "grip", "core"]),
]
NOTE_APPS = [("Notes", "Launch notes"), ("Obsidian", "Customer call"), ("Notion", "Weekly notes"), ("OneNote", "Meeting notes"), ("Notes", "Todo")]
for i, (text, n, retain) in enumerate(POS):
    app = NOTE_APPS[i % len(NOTE_APPS)]
    add(f"v2-pos-{i:02d}", text, app, {"shape": "dash_bullets", "min_bullets": n, "must_retain": retain}, "multi-point dictation in a notes surface")

# ---- Slice 2: 25 negative controls ----
NEG = [
    ("remind me to email the supplier about the delayed pallet tomorrow morning", ("Notes", "Todo"), ["supplier", "pallet"]),
    ("the meeting moved to thursday at four", ("Notes", "Calendar"), ["thursday", "four"]),
    ("i think the new landing page headline is finally right", ("Notes", "Ideas"), ["headline"]),
    ("yeah im gonna be like ten minutes late to the standup sorry", ("Slack", "#eng"), ["sorry", "ten minutes"]),
    ("lol that demo actually worked first try", ("Discord", "#general"), ["demo"]),
    ("can you resend the figma link the old one expired", ("Slack", "#design"), ["figma"]),
    ("sounds good see you at the cafe at noon", ("WhatsApp", "Maya"), ["noon"]),
    ("hey can you send me the invoice for march also let me know if the payment went through thanks", ("Gmail", "Compose"), ["invoice", "march"]),
    ("thanks for the quick turnaround the samples look great and we would like to order the full run", ("Outlook", "Reply"), ["samples", "full run"]),
    ("following up on my email from last week about the warranty claim", ("Gmail", "Compose"), ["warranty"]),
    ("we regret to inform you that the event has been postponed to october", ("Outlook", "Compose"), ["october"]),
    ("set the retry limit flag to eight and point the base url at localhost colon three three zero one", ("VS Code", "config.ts"), ["8", "3301"]),
    ("rename the get user function to fetch account and export it from the index", ("VS Code", "api.ts"), ["fetch", "index"]),
    ("run the migration then restart the worker pool", ("Terminal", "deploy"), ["migration", "worker"]),
    ("git commit dash m fix the race in the wake listener", ("Terminal", "shell"), ["race", "wake listener"]),
    ("the quarterly numbers look softer than expected mostly because of returns", ("Word", "Q3 report"), ["returns"]),
    ("please review section four for the liability language before friday", ("Word", "Contract"), ["section four", "friday"]),
    ("add a row for the hamburg office with headcount twelve", ("Excel", "Headcount"), ["hamburg", "twelve"]),
    ("what is the cheapest direct flight to austin next tuesday", ("Chrome", "Google"), ["austin", "tuesday"]),
    ("order more of the black large hoodies before the weekend", ("Notes", "Todo"), ["black", "hoodies"]),
    ("tell the team the deploy is frozen until the license bug is fixed", ("Slack", "#eng"), ["deploy", "license"]),
    ("the dentist moved my appointment to the ninth at eight thirty", ("Notes", "Personal"), ["ninth", "eight thirty"]),
    ("draft a one line reply saying we accept the revised terms", ("Gmail", "Reply"), ["accept", "terms"]),
    ("the printer on the third floor is out of toner again", ("Teams", "#facilities"), ["toner", "third floor"]),
    ("ship the patch today and hold the feature update for monday", ("Slack", "#release"), ["patch", "monday"]),
]
for i, (text, app, retain) in enumerate(NEG):
    expects = {"shape": "prose", "max_bullets": 0, "must_retain": retain}
    if app[0] in ("Gmail", "Outlook"):
        expects["forbid_added"] = ["Subject:", "Dear ", "Best regards", "Sincerely", "[Name]"]
    add(f"v2-neg-{i:02d}", text, app, expects, "single-point / chat / email / code: must stay prose, no reshape")

# ---- Slice 3: 15 context-leak traps ----
LEAK = [
    ("send the deck by friday", ("Salesforce", "Opportunity: Acme Corp renewal $84,000", "next step", "")),
    ("looks good approve it", ("Jira", "PROJ-4412 Fix pill z-order regression", "comment", "")),
    ("ask them about the timeline", ("Gmail", "Re: Acquisition discussion CONFIDENTIAL", "reply", "")),
    ("add a note that we tested on windows eleven", ("TestRail", "Run 88: HeardRight 1.4 RC2", "notes", "")),
    ("yes tomorrow works", ("Calendar", "Board meeting prep with Priya Sharma", "response", "")),
    ("bump the priority to high", ("Linear", "HR-231 wake word false accepts in cafes", "comment", "")),
    ("tell him the offer stands", ("WhatsApp", "Rahul Mehta +91 98200 11223", "message", "")),
    ("summarize what we agreed", ("Zoom", "1:1 with CFO - runway and bridge round", "chat", "")),
    ("start the refund", ("Stripe", "Customer cus_9x2 refund $149 founders", "note", "")),
    ("push it to staging", ("GitHub", "PR #412 rotate updater signing key", "comment", "")),
    ("schedule the follow up for next week", ("Epic", "Patient: A. D'souza - lipid panel review", "instructions", "")),
    ("looks like a duplicate close it", ("Zendesk", "Ticket 7731 refund not received", "internal note", "")),
    ("accept their counter", ("DocuSign", "Lease amendment Unit 4B $2,350/mo", "note", "")),
    ("share the numbers with the team", ("Metabase", "Dashboard: July revenue by SKU", "caption", "")),
    ("remind me to review this tonight", ("Figma", "Onboarding redesign v3 - internal only", "comment", "")),
]
for i, (text, ctx) in enumerate(LEAK):
    add(f"v2-leak-{i:02d}", text, ctx, {"shape": "prose", "max_bullets": 0, "forbid_context_leak": True}, "context is signal, never content: window/app strings must not enter output")

# ---- Slice 4: 15 tone/literal cases ----
# Inputs are POST-L0 text, which is what L1 actually receives: the taxonomy
# defines L1's input as "final ASR after local deterministic polish", and L0
# already converts spoken symbols (dash -> -, underscore -> _, slash -> /,
# "at sign"/"dot com" -> @/.com) plus number forms. Feeding raw spoken forms
# here would test L0's job, not L1's. L1's obligation is to PRESERVE an
# already-normalized literal while fixing the grammar around it.
TONE = [
    ("the api endpoint is /v1/apps/heardright/releases use it for the check", ("VS Code", "docs.md"), ["/v1/apps/heardright/releases"], []),
    ("email me at adrian@heardright.app when the build is done", ("Gmail", "Compose"), ["adrian@heardright.app"], []),
    ("the discount code is founders-149 for the launch", ("Slack", "#sales"), ["founders-149"], []),
    ("set timeout to 1500 milliseconds it keeps timing out", ("VS Code", "config.ts"), ["1500"], []),
    ("the build is version 0.1.12 not the older one", ("Slack", "#release"), ["0.1.12"], []),
    ("meet at 3:45 PM not four", ("WhatsApp", "Maya"), ["3:45"], []),
    ("the wake word is zephyr and it stays zephyr", ("Notes", "Positioning"), ["zephyr"], []),
    ("transfer 47,000 rupees to the vendor account today", ("Notes", "Todo"), ["47,000"], []),
    ("the file is report_final_v2.csv send that one", ("Slack", "#ops"), ["report_final_v2.csv"], []),
    ("password reset link expires in 24 hours please tell them", ("Gmail", "Compose"), ["24 hours"], ["Subject:", "Dear "]),
    ("call me back at 555-0123 anytime today", ("Notes", "Personal"), ["555-0123"], []),
    ("the sku is TS-HOOD-BLK-L we are out of stock", ("Excel", "Inventory"), ["TS-HOOD-BLK-L"], []),
    ("ship it no notes", ("Slack", "#design"), ["ship it"], []),
    ("honestly the margins are shit we need to renegotiate", ("Notes", "Supplier"), ["shit", "renegotiate"], []),
    ("tell support the fix lands in 1.5 not 2.0", ("Slack", "#support"), ["1.5"], []),
]
for i, (text, app, retain, forbid) in enumerate(TONE):
    expects = {"shape": "prose", "max_bullets": 0, "must_retain": retain}
    if forbid:
        expects["forbid_added"] = forbid
    add(f"v2-tone-{i:02d}", text, app, expects, "per-app tone + spoken-form literals must normalize and survive")

out = Path(__file__).parent / "shaping-eval-v2.jsonl"
out.write_text("\n".join(json.dumps(row, ensure_ascii=False) for row in ROWS) + "\n", "utf-8")
print(json.dumps({"rows": len(ROWS), "pos": 25, "neg": 25, "leak": 15, "tone": 15, "path": str(out)}))
