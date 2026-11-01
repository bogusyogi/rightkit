#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptFile = fileURLToPath(import.meta.url);
const scriptDir = path.dirname(scriptFile);
const repoRoot = path.resolve(scriptDir, "../../..");
const corpusDir = path.resolve(scriptDir, "../../bakeoff/l1-four-family-training-v1/corpus-candidates");
const corpusFile = path.join(corpusDir, "long-context.synthetic.jsonl");
const auditFile = path.join(corpusDir, "long-context.synthetic.assembly-audit.json");
const summaryFile = path.join(corpusDir, "long-context.synthetic.validation.json");
const deniedFile = path.join(corpusDir, "long-context.denied-manifest.json");
const schemaVersion = "heardright.l1.scheduled-static15.candidate.v2";

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function writeAtomic(file, contents) {
  const temporary = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, contents);
  fs.renameSync(temporary, file);
}

function words(value) {
  return String(value).toLocaleLowerCase("en-US").match(/[\p{L}\p{N}']+/gu) ?? [];
}

function normalized(value) {
  return words(value).join(" ");
}

function tokenEditSimilarity(left, right) {
  const a = words(left);
  const b = words(right);
  let prior = Array.from({ length: b.length + 1 }, (_, index) => index);
  for (let i = 1; i <= a.length; i += 1) {
    const current = [i];
    for (let j = 1; j <= b.length; j += 1) current[j] = Math.min(prior[j] + 1, current[j - 1] + 1, prior[j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1));
    prior = current;
  }
  const scale = Math.max(a.length, b.length, 1);
  return 1 - prior[b.length] / scale;
}

function ngramCounts(value, size = 5) {
  const tokens = words(value);
  const result = new Map();
  for (let index = 0; index + size <= tokens.length; index += 1) {
    const gram = tokens.slice(index, index + size).join(" ");
    result.set(gram, (result.get(gram) ?? 0) + 1);
  }
  return result;
}

function appendWithOverlap(left, right, maxCharacters = 16) {
  if (!left) return right;
  if (!right) return left;
  let overlap = 0;
  for (let length = 1; length <= Math.min(left.length, right.length, maxCharacters); length += 1) {
    if (left.slice(-length).toLocaleLowerCase("en-US") === right.slice(0, length).toLocaleLowerCase("en-US")) overlap = length;
  }
  const remainder = right.slice(overlap);
  return `${left}${overlap || /\s$/u.test(left) || /^\s/u.test(remainder) ? "" : " "}${remainder}`;
}

function seededShuffle(values, seed) {
  const result = [...values];
  let state = seed >>> 0;
  for (let index = result.length - 1; index > 0; index -= 1) {
    state = (state * 1664525 + 1013904223) >>> 0;
    const swap = Math.floor((state / 0x1_0000_0000) * (index + 1));
    [result[index], result[swap]] = [result[swap], result[index]];
  }
  return result;
}

const primaryCounts = Object.freeze({
  no_op_or_intentional_repetition: 90,
  contextual_semantic_repair: 60,
  multi_error_repair: 75,
  false_start_or_self_correction: 52,
  protected_span: 75,
  misleading_context: 52,
  boundary_or_overlap: 23,
  truncation_or_deletion_adversarial: 12,
  loop_adversarial: 11,
});

const people = ["Mara", "Ilan", "Tessa", "Oren", "Sana", "Jules", "Kavi", "Nora", "Bela", "Remy", "Zoya", "Amal"];
const products = ["Harbor", "Cinder", "Northstar", "Juniper", "Beacon", "Kestrel", "Solace", "Rivet", "Atlas", "Lumen", "Morrow", "Tandem"];
const topics = ["delivery plan", "renewal summary", "client update", "project brief", "support reply", "pricing memo", "launch checklist", "contract review", "design handoff", "migration note", "incident follow-up", "budget outline"];
const tailPhrases = [
  "Send the update after lunch", "Put the revised time on tomorrow's agenda", "Share the figure with the afternoon team", "Leave a copy in the project folder",
  "Call me when the supplier replies", "Add the address to the calendar invite", "Read the total back before sending", "Move the follow-up to Friday morning",
  "Tell the front desk about the change", "File the receipt with this month's expenses", "Bring the sample to the planning meeting", "Ask the editor to confirm the caption",
  "Post the route in the staff channel", "Save the measurements beside the floor plan", "Send the parent group a short reminder", "Ring the venue before the doors open",
  "Put the final count in the shift log", "Forward the signed page to the coordinator", "Book the smaller room for the workshop", "Mark the replacement part as urgent",
  "Send the latest draft to the producer", "Add the reference number to the case note", "Call the lab when the results arrive", "Leave the keys with the evening manager",
];
const domains = [
  { id: "logistics", actor: "dock coordinator", artifact: "loading schedule", location: "west depot", event: "afternoon carrier slot", issue: "two pallets arrived after the morning cutoff", outcome: "move the unload to bay four" },
  { id: "accounting", actor: "accounts clerk", artifact: "expense ledger", location: "finance office", event: "month-end review", issue: "the catering receipt is missing its tax line", outcome: "hold reimbursement until the vendor replies" },
  { id: "legal", actor: "contract manager", artifact: "supplier addendum", location: "review room", event: "signature meeting", issue: "the delivery clause still names the old warehouse", outcome: "circulate one corrected page" },
  { id: "design", actor: "art director", artifact: "cover layout", location: "studio", event: "client presentation", issue: "the photograph crops too close to the title", outcome: "use the wider proof for review" },
  { id: "software", actor: "release engineer", artifact: "deployment checklist", location: "operations desk", event: "evening rollout", issue: "the health check pauses on the second region", outcome: "delay traffic switching for ten minutes" },
  { id: "education", actor: "course coordinator", artifact: "workshop roster", location: "training room", event: "Saturday class", issue: "three learners requested the earlier session", outcome: "open another table near the window" },
  { id: "hospitality", actor: "venue manager", artifact: "banquet plan", location: "garden hall", event: "wedding breakfast", issue: "the florist needs access before the kitchen team", outcome: "unlock the side entrance at seven" },
  { id: "retail", actor: "store supervisor", artifact: "stock transfer", location: "riverside branch", event: "weekend promotion", issue: "the blue jackets were packed in the wrong sizes", outcome: "swap one carton with the central shop" },
  { id: "research", actor: "study coordinator", artifact: "sample log", location: "materials lab", event: "Tuesday analysis", issue: "one vial has a smudged collection time", outcome: "photograph the label before testing" },
  { id: "facilities", actor: "site engineer", artifact: "maintenance order", location: "north building", event: "lift inspection", issue: "the service panel rattles above the third floor", outcome: "keep the car offline until noon" },
  { id: "media", actor: "segment producer", artifact: "running order", location: "recording booth", event: "evening programme", issue: "the interview is four minutes longer than planned", outcome: "shorten the weather introduction" },
  { id: "community", actor: "volunteer lead", artifact: "collection list", location: "neighbourhood centre", event: "food drive", issue: "the first van cannot carry the folding tables", outcome: "send the equipment with the second driver" },
];
const registers = ["email", "team_chat", "meeting_note", "field_report", "customer_reply", "voice_memo", "case_note", "briefing"];
const discourseFrames = [
  { id: "chronology", build: (d) => [`Earlier, the ${d.actor} checked the ${d.artifact}.`, `Then ${d.issue}.`, `After that, the team agreed to ${d.outcome}.`, `By late afternoon, the ${d.location} had a workable sequence.`, `The ${d.event} can proceed once the last call is logged.`] },
  { id: "contrast", build: (d) => [`The ${d.artifact} looks complete at first glance, but ${d.issue}.`, `The ${d.actor} is ready to continue, although the timing remains tight.`, `The ${d.location} can absorb a short delay, whereas the next booking cannot.`, `A quick fix would be convenient, but the safer choice is to ${d.outcome}.`, `That leaves the ${d.event} on the original day.`] },
  { id: "cause_effect", build: (d) => [`Because ${d.issue}, the ${d.event} needs a small adjustment.`, `The ${d.actor} checked the effect on the ${d.artifact}.`, `That change frees the ${d.location} for the earlier task.`, `As a result, the team can ${d.outcome}.`, `Nobody else needs to alter their booking.`] },
  { id: "conditional", build: (d) => [`If ${d.issue}, call the ${d.actor} before changing the ${d.artifact}.`, `If the timing holds, the ${d.event} can start normally.`, `Otherwise, ask the ${d.location} to ${d.outcome}.`, `The backup choice costs less than cancelling the whole slot.`, `Confirm the decision after the next update.`] },
  { id: "question_answer", build: (d) => [`Do we need to change the ${d.event}?`, `The ${d.actor} says no, provided we ${d.outcome}.`, `What caused the concern was that ${d.issue}.`, `The ${d.artifact} already reflects the rest of the plan.`, `A short confirmation from the ${d.location} should settle it.`] },
  { id: "narrative", build: (d) => [`When the ${d.actor} opened the ${d.artifact}, everything appeared routine.`, `A few minutes later, ${d.issue}.`, `The team walked through the ${d.location} and compared the available options.`, `They chose to ${d.outcome}.`, `The ${d.event} stayed on course.`] },
  { id: "priority_list", build: (d) => [`First, check the ${d.artifact} with the ${d.actor}.`, `Second, note that ${d.issue}.`, `Third, ask the ${d.location} to ${d.outcome}.`, `Finally, confirm the revised sequence for the ${d.event}.`, `The remaining work can wait until tomorrow.`] },
  { id: "correction", build: (d) => [`I thought the ${d.artifact} was final, but that was premature.`, `The actual problem is that ${d.issue}.`, `The ${d.actor} found it during a last check at the ${d.location}.`, `So the team will ${d.outcome}.`, `That correction is enough for the ${d.event}.`] },
  { id: "rationale", build: (d) => [`Please ${d.outcome} because ${d.issue}.`, `The ${d.actor} compared that option with changing the entire ${d.artifact}.`, `It causes less disruption at the ${d.location}.`, `It also protects the timing of the ${d.event}.`, `A brief note explaining the choice will be enough.`] },
  { id: "status_update", build: (d) => [`Here is the current position on the ${d.artifact}.`, `The ${d.actor} has completed the routine checks.`, `The open point is that ${d.issue}.`, `The proposed response is to ${d.outcome}.`, `The ${d.event} remains scheduled.`] },
  { id: "comparison", build: (d) => [`We compared two options for the ${d.artifact}.`, `Changing the whole plan would disrupt the ${d.location}.`, `The smaller option is to ${d.outcome}.`, `That works even though ${d.issue}.`, `The ${d.actor} prefers it for the ${d.event}.`] },
  { id: "handoff", build: (d) => [`For the next shift, the ${d.artifact} is the main handoff item.`, `The ${d.actor} has already noted that ${d.issue}.`, `The team at the ${d.location} should ${d.outcome}.`, `The ${d.actor} can call if the timing changes again.`, `The ${d.event} needs no other adjustment.`] },
];
const counterfactualFrames = [
  (d, person, product) => `An older ${d.artifact} names ${product}, but it belongs to last month's archive.`,
  (d, person) => `The sidebar lists ${person} as owner of a closed ${d.event}.`,
  (d, person, product) => `A completed task mentions ${person} and ${product} in a different account.`,
  (d) => `The search preview shows yesterday's note from the ${d.location}.`,
  (d, person) => `${person} appears in a nearby comment about an unrelated ${d.artifact}.`,
  (d, person, product) => `The archived result pairs ${product} with ${person}, not with this request.`,
];
const structuralCaps = Object.freeze({ max_rows_per_frame: 6, max_rows_per_sentence: 8, max_rows_per_sixgram: 24, max_sixgram_corpus_share: 0.06 });

const generatorHash = sha256(fs.readFileSync(scriptFile));
if (!fs.existsSync(deniedFile)) throw new Error(`denied manifest missing: ${deniedFile}`);
const deniedRaw = fs.readFileSync(deniedFile);
const deniedManifest = JSON.parse(deniedRaw);
const deniedManifestHash = sha256(deniedRaw);
if (!deniedManifest.denied_entities?.length || !deniedManifest.denied_phrases?.length) throw new Error("denied manifest must be non-empty");
if (deniedManifest.schema !== "heardright.l1_long_context_benchmark_deny_manifest.v2" || deniedManifest.status !== "fail_closed") throw new Error("denied manifest schema/status mismatch");
if (!Array.isArray(deniedManifest.source_files) || deniedManifest.source_files.length !== 10) throw new Error("denied manifest must bind ten authoritative sources");
for (const source of deniedManifest.source_files) {
  const absolute = path.resolve(repoRoot, source.path);
  if (!fs.existsSync(absolute)) throw new Error(`denied source missing: ${source.path}`);
  const actual = sha256(fs.readFileSync(absolute));
  if (actual !== source.sha256) throw new Error(`denied source changed: ${source.path}`);
}
const templateCatalogHash = sha256(JSON.stringify({
  version: 3,
  domains,
  registers,
  discourse_frames: discourseFrames.map((frame) => frame.id),
  tails: tailPhrases,
  counterfactual_frame_count: counterfactualFrames.length,
  structural_caps: structuralCaps,
}));

function labelSequence() {
  return seededShuffle(Object.entries(primaryCounts).flatMap(([label, count]) => Array(count).fill(label)), 0x51A7_2026);
}

function sessionShapes() {
  const medium = [...Array(39).fill(2), ...Array(39).fill(3)].map((unitCount, index) => ({
    band: "medium_16_45s", unitCount, duration_s: 16 + (index * 7) % 30, detailCount: 1,
  }));
  const long = Array.from({ length: 33 }, (_, index) => ({
    band: "long_46_90s", unitCount: 5, duration_s: 46 + (index * 11) % 45, detailCount: 3,
  }));
  const veryLong = [...Array(6).fill(7), ...Array(6).fill(8)].map((unitCount, index) => ({
    band: "very_long_91_180s", unitCount, duration_s: 91 + (index * 17) % 90, detailCount: 5,
  }));
  return [...medium, ...long, ...veryLong];
}

function arrangeSessionLabels(labels) {
  if (labels[0] !== "boundary_or_overlap") return labels;
  const swap = labels.findIndex((label, index) => index > 0 && label !== "boundary_or_overlap");
  if (swap < 0) throw new Error("session contains no legal non-boundary first row");
  [labels[0], labels[swap]] = [labels[swap], labels[0]];
  return labels;
}

function naturalDetails(index, count, domain, discourse) {
  const domainSignals = [domain.actor, domain.artifact, domain.location, domain.event];
  const sentences = discourse.build(domain).map((sentence) => {
    if (domainSignals.some((signal) => sentence.includes(signal))) return sentence;
    return `${sentence.slice(0, -1)} for the ${domain.artifact}.`;
  });
  const offset = Math.floor((index - 1) / 144) % sentences.length;
  return Array.from({ length: count }, (_, position) => sentences[(offset + position) % sentences.length]).join(" ");
}

function denseContext(label, index, band, person, product, caseCode, domain, register) {
  const dense = band !== "medium_16_45s" || label === "contextual_semantic_repair" || label === "misleading_context";
  const context = {
    reference_only: true,
    app_kind: register,
    app_window_title: dense ? `${product} ${domain.artifact}` : `${product} message`,
    focused_control: { role: "text box", label: dense ? `${domain.event} update` : "Draft message", value: "" },
    nearby_accessibility_nodes: [],
    visible_labels: [],
    document_selection_excerpt: "",
    recent_repaired_text: "",
    counterfactual_context: [],
    context_resolution_terms: [],
  };
  if (dense) {
    const nodeText = [domain.actor, domain.artifact, domain.location, domain.event, domain.issue, domain.outcome, `case ${caseCode}`, `${product} archive`];
    context.nearby_accessibility_nodes = nodeText.map((text, offset) => ({ role: offset % 2 ? "label" : "group", text }));
    const labelRoots = ["Overview", "Schedule", "Owner", "Location", "Notes", "History", "Attachments", "Status", "Calendar", "Archive", "Comments", "Reference"];
    context.visible_labels = labelRoots.map((root, offset) => `${root} ${1 + ((index + offset) % 9)}`);
    context.document_selection_excerpt = `A previous ${domain.artifact} from the ${domain.location} describes ${domain.issue}. The entry is dated last month and belongs to a closed ${domain.event}.`;
    context.recent_repaired_text = `${domain.actor} confirmed the earlier appointment at the ${domain.location}.`;
  }
  if (label === "contextual_semantic_repair") {
    context.focused_control.label = "Invoice response";
    context.context_resolution_terms = ["invoice"];
    context.counterfactual_context = [counterfactualFrames[index % counterfactualFrames.length](domain, people[(index + 5) % people.length], products[(index + 2) % products.length])];
  }
  if (label === "misleading_context") {
    const otherPerson = people[(index + 5) % people.length];
    const otherProduct = products[(index + 4) % products.length];
    context.counterfactual_context = [
      counterfactualFrames[index % counterfactualFrames.length](domain, otherPerson, otherProduct),
      counterfactualFrames[(index + 3) % counterfactualFrames.length](domain, people[(index + 8) % people.length], products[(index + 7) % products.length]),
    ];
  }
  return { context, density: dense ? "dense" : "light" };
}

function applyPermittedEdits(source, edits) {
  let output = source;
  for (const edit of edits) {
    if (edit.op === "replace_literal" || edit.op === "delete_literal") {
      const from = edit.from;
      const matches = output.split(from).length - 1;
      if (matches !== 1) throw new Error(`${edit.op} expected one literal '${from}', found ${matches}`);
      output = output.replace(from, edit.op === "delete_literal" ? "" : edit.to);
    } else if (edit.op === "capitalize_first") {
      output = output ? `${output[0].toLocaleUpperCase("en-US")}${output.slice(1)}` : output;
    } else if (edit.op === "capitalize_after_overlap") {
      const index = output.indexOf(edit.after);
      if (index !== 0) throw new Error("overlap capitalization anchor missing");
      const position = edit.after.length;
      output = `${output.slice(0, position)}${output[position].toLocaleUpperCase("en-US")}${output.slice(position + 1)}`;
    } else if (edit.op === "append_terminal") {
      if (!/[.!?]$/u.test(output)) output += edit.mark;
    } else {
      throw new Error(`unknown permitted edit ${edit.op}`);
    }
  }
  return output.replace(/ {2,}/gu, " ").trim();
}

function spanFor(input, text, kind) {
  const start = input.indexOf(text);
  if (start < 0) throw new Error(`span '${text}' missing from input`);
  return { start, end: start + text.length, text, kind };
}

function editSpans(input, edits) {
  return edits.map((edit) => {
    if (!edit.from) return { op: edit.op, input_span: null, reason: edit.reason };
    const start = input.indexOf(edit.from);
    if (start < 0) throw new Error(`edit source '${edit.from}' missing`);
    return { op: edit.op, input_span: { start, end: start + edit.from.length, text: edit.from }, replacement: edit.to ?? "", reason: edit.reason };
  });
}

function compose(label, index, shape, { leadingOverlap = null, trailingOverlap = null } = {}) {
  const person = people[index % people.length];
  const product = products[(index * 5) % products.length];
  const topic = topics[(index * 7) % topics.length];
  const domain = domains[(index - 1) % domains.length];
  const discourse = discourseFrames[Math.floor((index - 1) / domains.length) % discourseFrames.length];
  const register = registers[Math.floor((index - 1) / 36) % registers.length];
  const caseCode = `SX-${String(7100 + index).padStart(4, "0")}`;
  const time = `${(index % 9) + 1}:${index % 2 ? "30" : "00"}`;
  const due = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"][index % 5];
  const tail = `${tailPhrases[(index * 11) % tailPhrases.length]} regarding ${caseCode} and the ${domain.artifact}`;
  let detailCount = shape.detailCount;
  let detail = naturalDetails(index, detailCount, domain, discourse);
  const edits = [];
  const protectedTexts = [];
  let core;
  switch (label) {
    case "no_op_or_intentional_repetition":
      core = index % 2
        ? `no no keep the ${topic} for ${product} under ${caseCode}`
        : `please keep the ${topic} for ${product} under ${caseCode}`;
      protectedTexts.push([caseCode, "identifier"]);
      break;
    case "contextual_semantic_repair":
      core = `send the inverse number ${caseCode} to ${person}`;
      edits.push({ op: "replace_literal", from: "inverse", to: "invoice", reason: "context-supported acoustic repair" });
      protectedTexts.push([caseCode, "identifier"], [person, "name"]);
      break;
    case "multi_error_repair":
      core = `send ${person} the ${product.toLocaleLowerCase("en-US")} ${topic} on ${due.toLocaleLowerCase("en-US")} at ${time.replace(":", " ")} and mark ${caseCode} high priority`;
      edits.push(
        { op: "replace_literal", from: product.toLocaleLowerCase("en-US"), to: product, reason: "proper-name casing" },
        { op: "replace_literal", from: due.toLocaleLowerCase("en-US"), to: due, reason: "day-name casing" },
        { op: "replace_literal", from: time.replace(":", " "), to: time, reason: "dictated time formatting" },
      );
      protectedTexts.push([person, "name"], [caseCode, "identifier"]);
      break;
    case "false_start_or_self_correction":
      {
        const falseStarts = ["I need to um I mean ", "Actually wait no ", "I was going to say sorry ", "Let me start that again "];
        const falseStart = falseStarts[index % falseStarts.length];
        core = `${falseStart}please move the ${topic} for ${product} under ${caseCode} to ${due}`;
        edits.push({ op: "delete_literal", from: falseStart, reason: "explicit false start" });
      }
      protectedTexts.push([caseCode, "identifier"]);
      break;
    case "protected_span": {
      const urlSpoken = `https colon slash slash ${product.toLocaleLowerCase("en-US")} dot example slash ${caseCode.toLocaleLowerCase("en-US")}`;
      const protectedForms = [
        `tell ${person} that ${caseCode} stays at 12 percent and the url for ${product} is ${urlSpoken}`,
        `for ${caseCode} give ${person} the 12 percent figure and use the ${product} url at this address, ${urlSpoken}`,
        `reply to ${person} about ${caseCode} with 12 percent and include ${product}'s url recorded as ${urlSpoken}`,
        `put 12 percent beside ${caseCode}, note the ${product} url through ${urlSpoken}, and notify ${person}`,
      ];
      core = protectedForms[index % protectedForms.length];
      edits.push(
        { op: "replace_literal", from: "12 percent", to: "12%", reason: "percentage formatting" },
        { op: "replace_literal", from: "url", to: "URL", reason: "initialism casing" },
        { op: "replace_literal", from: urlSpoken, to: `https://${product.toLocaleLowerCase("en-US")}.example/${caseCode.toLocaleLowerCase("en-US")}`, reason: "dictated URL formatting" },
      );
      protectedTexts.push([person, "name"], [caseCode, "identifier"], ["12", "number"]);
      break;
    }
    case "misleading_context":
      core = `tell ${person} the ${product} build under ${caseCode} is ready`;
      protectedTexts.push([person, "name"], [product, "product"], [caseCode, "identifier"]);
      break;
    case "boundary_or_overlap":
      core = `the ${product} ${topic} under ${caseCode} should go to ${person}`;
      protectedTexts.push([person, "name"], [caseCode, "identifier"]);
      break;
    case "truncation_or_deletion_adversarial":
      core = `approve ${caseCode} for ${person} but do not change the renewal amount or remove the final exception`;
      protectedTexts.push([caseCode, "identifier"], [person, "name"]);
      break;
    case "loop_adversarial":
      core = `no no no do not resend ${caseCode} to ${person}`;
      protectedTexts.push([caseCode, "identifier"], [person, "name"]);
      break;
    default:
      throw new Error(`unknown label ${label}`);
  }
  const prefix = leadingOverlap ? `${leadingOverlap} ` : "";
  const closing = trailingOverlap ? `${tail}; ${trailingOverlap.slice(0, -1)}` : tail;
  let input = `${prefix}${core}. ${detail} ${closing}`.replace(/ {2,}/gu, " ").trim();
  while (words(input).length > 100 && detailCount > 1) {
    detailCount -= 1;
    detail = naturalDetails(index, detailCount, domain, discourse);
    input = `${prefix}${core}. ${detail} ${closing}`.replace(/ {2,}/gu, " ").trim();
  }
  if (words(input).length < 24) input = `${input}. ${person} will call about ${caseCode} from the ${domain.location} at ${time}`;
  if (leadingOverlap) edits.push({ op: "capitalize_after_overlap", after: `${leadingOverlap} `, reason: "sentence casing after retained overlap" });
  edits.push({ op: "capitalize_first", reason: "sentence casing" });
  edits.push({ op: "append_terminal", mark: ".", reason: "terminal punctuation" });
  const candidate = applyPermittedEdits(input, edits);
  const contextResult = denseContext(label, index, shape.band, person, product, caseCode, domain, register);
  const templateDescriptor = {
    version: 3,
    primary_label: label,
    domain: domain.id,
    register,
    discourse: discourse.id,
    duration_band: shape.band,
    detail_sentences: detailCount,
    core_variant: label === "no_op_or_intentional_repetition" ? index % 2 : 0,
    tail_variant: (index * 11) % tailPhrases.length,
    overlap_role: leadingOverlap ? "leading" : trailingOverlap ? "trailing" : "none",
  };
  return {
    input,
    candidate,
    edits,
    protectedSpans: protectedTexts.map(([text, kind]) => spanFor(input, text, kind)),
    tailAnchor: tail,
    context: contextResult.context,
    contextDensity: contextResult.density,
    caseCode,
    domain: domain.id,
    register,
    discourse: discourse.id,
    templateDescriptor,
    templateHash: sha256(JSON.stringify(templateDescriptor)),
    structuralFrameId: `${label}|${domain.id}|${register}|${discourse.id}|${shape.band}|${shape.detailCount}|${templateDescriptor.core_variant}|${templateDescriptor.overlap_role}`,
  };
}

function deniedMatches(row) {
  const haystack = normalized(JSON.stringify({ input: row.input_text, output: row.candidate_output, context: row.bounded_context }));
  const matches = [];
  for (const entity of deniedManifest.denied_entities) {
    const needle = normalized(entity);
    if (` ${haystack} `.includes(` ${needle} `)) matches.push({ type: "entity", value: entity });
  }
  const policy = deniedManifest.near_match_policy;
  for (const phrase of deniedManifest.denied_phrases) {
    const phraseTokens = words(phrase);
    const needle = phraseTokens.join(" ");
    if (haystack.includes(needle)) {
      matches.push({ type: "phrase", value: phrase });
      continue;
    }
    if (phraseTokens.length < policy.minimum_phrase_tokens) continue;
    const sourceTokens = words(haystack);
    for (let size = Math.max(policy.minimum_phrase_tokens, phraseTokens.length - 2); size <= phraseTokens.length + 2; size += 1) {
      for (let start = 0; start + size <= sourceTokens.length; start += 1) {
        const windowTokens = sourceTokens.slice(start, start + size);
        const window = new Set(windowTokens);
        const denied = new Set(phraseTokens);
        const intersection = [...window].filter((token) => denied.has(token)).length;
        const union = new Set([...window, ...denied]).size;
        const jaccard = union ? intersection / union : 0;
        const editSimilarity = tokenEditSimilarity(windowTokens.join(" "), phrase);
        if (jaccard >= policy.token_jaccard_max || editSimilarity >= policy.normalized_edit_similarity_max) matches.push({ type: "near_phrase", value: phrase });
      }
    }
  }
  return matches;
}

function buildRows() {
  const labels = labelSequence();
  const rows = [];
  let cursor = 0;
  let index = 1;
  for (const [sessionIndex, shape] of sessionShapes().entries()) {
    const sessionId = `synthetic-authored-ss15-${String(sessionIndex + 1).padStart(3, "0")}`;
    const sessionLabels = arrangeSessionLabels(labels.slice(cursor, cursor + shape.unitCount));
    cursor += shape.unitCount;
    const leadingOverlaps = Array(shape.unitCount).fill(null);
    const trailingOverlaps = Array(shape.unitCount).fill(null);
    for (let position = 1; position < sessionLabels.length; position += 1) {
      if (sessionLabels[position] === "boundary_or_overlap") {
        const overlap = `handoff ${sessionIndex + 1}.`;
        if (overlap.length > 16) throw new Error(`overlap exceeds 16 characters: ${overlap}`);
        leadingOverlaps[position] = overlap;
        trailingOverlaps[position - 1] = overlap;
      }
    }
    for (let position = 0; position < shape.unitCount; position += 1) {
      const label = sessionLabels[position];
      const item = compose(label, index, shape, { leadingOverlap: leadingOverlaps[position], trailingOverlap: trailingOverlaps[position] });
      const rowId = `ss15-synthetic-candidate-${String(index).padStart(4, "0")}`;
      const boundedContextJson = JSON.stringify(item.context);
      const row = {
        schema_version: schemaVersion,
        lifecycle: "candidate",
        row_id: rowId,
        logical_cluster_id: `${sessionId}-chunk-${position + 1}`,
        session_cluster_id: sessionId,
        split: "adjudication_candidate",
        training_eligible: false,
        source_kind: "synthetic_authored_text",
        source_template_id: `synthetic-authored.${label}.v3`,
        synthetic_seed: 0x51A72026 + index,
        consent: "not_applicable_synthetic",
        license: "internal_synthetic",
        duration_band: shape.band,
        duration_s: shape.duration_s,
        scheduled_background_windows: shape.unitCount - 1,
        tail_audio_s: Number((4.5 + (sessionIndex % 9)).toFixed(1)),
        is_stop_tail: position === shape.unitCount - 1,
        chunk_index: position + 1,
        chunk_count: shape.unitCount,
        input_text: item.input,
        current_l0_chunk: item.input,
        candidate_output: item.candidate,
        gold_output: null,
        cloud_l1_draft: null,
        bounded_context: item.context,
        context_relevance: label === "contextual_semantic_repair" ? "helpful" : label === "misleading_context" ? "misleading" : "none",
        context_density: item.contextDensity,
        domain: item.domain,
        register: item.register,
        discourse_structure: item.discourse,
        boundary_overlap_source: leadingOverlaps[position]
          ? { text: leadingOverlaps[position], prior_row_id: `ss15-synthetic-candidate-${String(index - 1).padStart(4, "0")}`, max_characters: 16 }
          : null,
        structured_spans: {
          protected: item.protectedSpans,
          permitted_changes: editSpans(item.input, item.edits),
          deletion_permissions: item.edits.filter((edit) => edit.op === "delete_literal").map((edit) => spanFor(item.input, edit.from, "approved_false_start_candidate")),
          context_resolution: item.context.context_resolution_terms.map((text) => ({ text, basis: "bounded_reference_context" })),
        },
        permitted_edit_ops: item.edits,
        source_tail_anchor: item.tailAnchor,
        failure_tags: [label],
        input_word_count: words(item.input).length,
        serialized_input_word_count: words(`${item.input} ${boundedContextJson}`).length,
        adjudication_status: "pending",
        adjudication: {
          status: "pending",
          primary_reviewer: null,
          secondary_reviewer: null,
          decision: null,
          rationale: null,
          reviewed_at: null,
          cloud_l1_comparison: null,
        },
        serializer_version: "scheduled-static15-context-v2",
        teacher_prompt_hash: null,
        generator_sha256: generatorHash,
        template_catalog_sha256: templateCatalogHash,
        template_lineage: {
          descriptor: item.templateDescriptor,
          template_sha256: item.templateHash,
          structural_frame_id: item.structuralFrameId,
        },
        denied_manifest_sha256: deniedManifestHash,
      };
      row.source_hash = `sha256:${sha256(`${row.session_cluster_id}\n${row.input_text}\n${row.candidate_output}\n${row.synthetic_seed}\n${item.templateHash}`)}`;
      rows.push(row);
      index += 1;
    }
  }
  if (cursor !== labels.length) throw new Error(`unused labels: ${labels.length - cursor}`);
  return rows;
}

function distributionMetrics(rows) {
  const frames = new Map();
  const sentences = new Map();
  const sixgrams = new Map();
  const domainsSeen = new Map();
  const registersSeen = new Map();
  const discoursesSeen = new Map();
  for (const row of rows) {
    const frame = row.template_lineage.structural_frame_id;
    frames.set(frame, (frames.get(frame) ?? 0) + 1);
    domainsSeen.set(row.domain, (domainsSeen.get(row.domain) ?? 0) + 1);
    registersSeen.set(row.register, (registersSeen.get(row.register) ?? 0) + 1);
    discoursesSeen.set(row.discourse_structure, (discoursesSeen.get(row.discourse_structure) ?? 0) + 1);
    const rowSentences = new Set(row.input_text.split(/[.!?]+/u).map(normalized).filter((value) => words(value).length >= 5));
    for (const sentence of rowSentences) sentences.set(sentence, (sentences.get(sentence) ?? 0) + 1);
    const rowSixgrams = new Set(ngramCounts(row.input_text, 6).keys());
    for (const gram of rowSixgrams) sixgrams.set(gram, (sixgrams.get(gram) ?? 0) + 1);
  }
  const maximum = (map) => map.size ? Math.max(...map.values()) : 0;
  const top = (map, count = 5) => [...map.entries()].sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0])).slice(0, count).map(([value, rowsWithValue]) => ({ value, rows: rowsWithValue }));
  const maxSixgram = maximum(sixgrams);
  return {
    caps: structuralCaps,
    structural_frames: { distinct: frames.size, max_rows: maximum(frames), top: top(frames) },
    normalized_sentences: { distinct: sentences.size, max_document_frequency: maximum(sentences), top: top(sentences) },
    sixgrams: { distinct: sixgrams.size, max_document_frequency: maxSixgram, max_corpus_share: rows.length ? maxSixgram / rows.length : 0, top: top(sixgrams) },
    domains: Object.fromEntries([...domainsSeen].sort()),
    registers: Object.fromEntries([...registersSeen].sort()),
    discourse_structures: Object.fromEntries([...discoursesSeen].sort()),
  };
}

function buildAssemblyAudit(rows, corpusHash) {
  const groups = new Map();
  for (const row of rows) {
    const group = groups.get(row.session_cluster_id) ?? [];
    group.push(row);
    groups.set(row.session_cluster_id, group);
  }
  const sessions = [];
  for (const [sessionClusterId, group] of groups) {
    const ordered = [...group].sort((left, right) => left.chunk_index - right.chunk_index);
    const assembled = ordered.reduce((value, row) => appendWithOverlap(value, row.candidate_output), "");
    sessions.push({
      session_cluster_id: sessionClusterId,
      row_ids: ordered.map((row) => row.row_id),
      boundary_overlap_count: ordered.filter((row) => row.boundary_overlap_source).length,
      candidate_outputs_sha256: sha256(ordered.map((row) => row.candidate_output).join("\n")),
      assembled_candidate_output: assembled,
      assembled_candidate_output_sha256: sha256(assembled),
    });
  }
  return {
    schema: "heardright.l1.synthetic-assembly-audit.v1",
    corpus_sha256: corpusHash,
    generator_sha256: generatorHash,
    template_catalog_sha256: templateCatalogHash,
    denied_manifest_sha256: deniedManifestHash,
    session_count: sessions.length,
    sessions,
  };
}

function validate(rows, audit, { smoke = false } = {}) {
  const errors = [];
  const add = (condition, message) => { if (!condition) errors.push(message); };
  const expectedRows = smoke ? 2 : 450;
  add(rows.length === expectedRows, `row count ${rows.length}/${expectedRows}`);
  add(new Set(rows.map((row) => row.row_id)).size === rows.length, "duplicate row ids");
  add(new Set(rows.map((row) => normalized(row.input_text))).size === rows.length, "duplicate normalized inputs");
  const counts = Object.fromEntries(Object.keys(primaryCounts).map((label) => [label, 0]));
  const lengthBands = { medium_16_45s: 0, long_46_90s: 0, very_long_91_180s: 0 };
  let boundaryCount = 0;
  let denseContextCount = 0;
  let over40Words = 0;
  let minInputWords = Number.POSITIVE_INFINITY;
  let maxInputWords = 0;
  const retiredPolicyPhrases = ["preserve the direct tone", "must survive any cleanup", "should not disappear at a chunk boundary", "selected field expects plain text", "surrounding document is reference material"];
  const byId = new Map(rows.map((row) => [row.row_id, row]));
  for (const row of rows) {
    const label = row.failure_tags?.[0];
    counts[label] = (counts[label] ?? 0) + 1;
    lengthBands[row.duration_band] = (lengthBands[row.duration_band] ?? 0) + 1;
    if (row.context_density === "dense") denseContextCount += 1;
    if (row.input_word_count > 40) over40Words += 1;
    minInputWords = Math.min(minInputWords, row.input_word_count);
    maxInputWords = Math.max(maxInputWords, row.input_word_count);
    if (row.boundary_overlap_source) boundaryCount += 1;
    add(row.schema_version === schemaVersion && row.lifecycle === "candidate", `candidate schema ${row.row_id}`);
    add(row.split === "adjudication_candidate" && row.training_eligible === false, `training gate ${row.row_id}`);
    add(row.source_kind === "synthetic_authored_text", `source provenance ${row.row_id}`);
    add(row.adjudication_status === "pending" && row.adjudication?.status === "pending", `adjudication status ${row.row_id}`);
    add(row.gold_output === null && row.cloud_l1_draft === null, `target authority ${row.row_id}`);
    add(!Object.hasOwn(row, "session_proposed_output") && !Object.hasOwn(row, "prior_repaired_tail"), `forbidden row assembly field ${row.row_id}`);
    add(row.input_text === row.current_l0_chunk && typeof row.logical_cluster_id === "string", `input/cluster ${row.row_id}`);
    add(row.generator_sha256 === generatorHash && row.denied_manifest_sha256 === deniedManifestHash && row.template_catalog_sha256 === templateCatalogHash, `generator binding ${row.row_id}`);
    add(row.template_lineage.template_sha256 === sha256(JSON.stringify(row.template_lineage.descriptor)), `template lineage ${row.row_id}`);
    add(row.input_word_count >= 24 && row.input_word_count <= 100, `input word bounds ${row.row_id}:${row.input_word_count}`);
    add(row.input_text.split(/[.!?]+/u).filter((value) => words(value).length).length >= 2, `multi-sentence structure ${row.row_id}`);
    add(!retiredPolicyPhrases.some((phrase) => normalized(row.input_text).includes(normalized(phrase))), `retired policy padding ${row.row_id}`);
    add(applyPermittedEdits(row.input_text, row.permitted_edit_ops) === row.candidate_output, `edit replay ${row.row_id}`);
    add(typeof row.candidate_output === "string" && /[.!?]$/u.test(row.candidate_output), `complete output ${row.row_id}`);
    add(!/[\u2013\u2014]/u.test(row.candidate_output) && !/###/u.test(row.candidate_output), `output contract ${row.row_id}`);
    add(row.candidate_output.toLocaleLowerCase("en-US").includes(row.source_tail_anchor.toLocaleLowerCase("en-US")), `tail deletion ${row.row_id}`);
    for (const span of row.structured_spans.protected) {
      add(row.input_text.slice(span.start, span.end) === span.text, `protected source span ${row.row_id}:${span.text}`);
      add(row.candidate_output.includes(span.text), `protected output span ${row.row_id}:${span.text}`);
    }
    const sourceLoops = ngramCounts(row.input_text, 5);
    for (const [gram, count] of ngramCounts(row.candidate_output, 5)) {
      if (count > 1) add(count <= (sourceLoops.get(gram) ?? 0), `introduced loop ${row.row_id}:${gram}`);
    }
    add(JSON.stringify(row.bounded_context).length <= 2400, `total context cap ${row.row_id}`);
    add(row.bounded_context.reference_only === true, `context trust boundary ${row.row_id}`);
    add(row.bounded_context.app_window_title.length <= 80, `title cap ${row.row_id}`);
    add(JSON.stringify(row.bounded_context.focused_control).length <= 240, `focused control cap ${row.row_id}`);
    add(row.bounded_context.nearby_accessibility_nodes.length <= 8, `AX node cap ${row.row_id}`);
    add(row.bounded_context.visible_labels.length <= 12, `visible label cap ${row.row_id}`);
    add(row.bounded_context.document_selection_excerpt.length <= 320, `document excerpt cap ${row.row_id}`);
    add(row.bounded_context.recent_repaired_text.length <= 160, `recent text cap ${row.row_id}`);
    const denied = deniedMatches(row);
    add(denied.length === 0, `denied match ${row.row_id}:${JSON.stringify(denied.slice(0, 2))}`);
    if (row.boundary_overlap_source) {
      const prior = byId.get(row.boundary_overlap_source.prior_row_id);
      add(Boolean(prior), `boundary prior missing ${row.row_id}`);
      add(row.boundary_overlap_source.text.length <= 16, `boundary length ${row.row_id}`);
      if (prior) {
        add(prior.candidate_output.toLocaleLowerCase("en-US").endsWith(row.boundary_overlap_source.text.toLocaleLowerCase("en-US")), `boundary prior suffix ${row.row_id}`);
        add(row.candidate_output.toLocaleLowerCase("en-US").startsWith(row.boundary_overlap_source.text.toLocaleLowerCase("en-US")), `boundary current prefix ${row.row_id}`);
      }
    }
  }
  if (!smoke) {
    for (const [label, expected] of Object.entries(primaryCounts)) add(counts[label] === expected, `label ${label}: ${counts[label]}/${expected}`);
    add(lengthBands.medium_16_45s === 195 && lengthBands.long_46_90s === 165 && lengthBands.very_long_91_180s === 90, `length distribution ${JSON.stringify(lengthBands)}`);
    add(boundaryCount === 23, `boundary count ${boundaryCount}/23`);
    add(over40Words >= 255, `>40-word coverage ${over40Words}/255 minimum`);
    add(denseContextCount >= 255, `dense-context coverage ${denseContextCount}/255 minimum`);
  }
  const distributions = distributionMetrics(rows);
  if (!smoke) {
    add(distributions.structural_frames.max_rows <= structuralCaps.max_rows_per_frame, `structural frame concentration ${distributions.structural_frames.max_rows}`);
    add(distributions.normalized_sentences.max_document_frequency <= structuralCaps.max_rows_per_sentence, `sentence document frequency ${distributions.normalized_sentences.max_document_frequency}: ${JSON.stringify(distributions.normalized_sentences.top)}`);
    add(distributions.sixgrams.max_document_frequency <= structuralCaps.max_rows_per_sixgram, `sixgram document frequency ${distributions.sixgrams.max_document_frequency}: ${JSON.stringify(distributions.sixgrams.top)}`);
    add(distributions.sixgrams.max_corpus_share <= structuralCaps.max_sixgram_corpus_share, `sixgram corpus share ${distributions.sixgrams.max_corpus_share}: ${JSON.stringify(distributions.sixgrams.top)}`);
    add(Object.keys(distributions.domains).length === domains.length, `domain coverage ${Object.keys(distributions.domains).length}/${domains.length}`);
    add(Object.keys(distributions.registers).length === registers.length, `register coverage ${Object.keys(distributions.registers).length}/${registers.length}`);
    add(Object.keys(distributions.discourse_structures).length === discourseFrames.length, `discourse coverage ${Object.keys(distributions.discourse_structures).length}/${discourseFrames.length}`);
  }
  const expectedSessions = smoke ? 1 : 123;
  add(audit.generator_sha256 === generatorHash && audit.denied_manifest_sha256 === deniedManifestHash && audit.template_catalog_sha256 === templateCatalogHash, "assembly audit lineage");
  add(audit.session_count === expectedSessions && audit.sessions.length === expectedSessions, `audit sessions ${audit.session_count}/${expectedSessions}`);
  for (const session of audit.sessions) {
    const group = session.row_ids.map((rowId) => byId.get(rowId));
    add(group.every(Boolean), `audit row missing ${session.session_cluster_id}`);
    if (!group.every(Boolean)) continue;
    add(group.every((row, index) => row.chunk_index === index + 1 && row.chunk_count === group.length), `chunk order ${session.session_cluster_id}`);
    add(group.at(-1).is_stop_tail && group.slice(0, -1).every((row) => !row.is_stop_tail), `tail position ${session.session_cluster_id}`);
    const assembled = group.reduce((value, row) => appendWithOverlap(value, row.candidate_output), "");
    add(assembled === session.assembled_candidate_output, `assembly target ${session.session_cluster_id}`);
    add(sha256(assembled) === session.assembled_candidate_output_sha256, `assembly hash ${session.session_cluster_id}`);
  }
  if (errors.length) throw new Error(`validation failed (${errors.length}): ${errors.slice(0, 30).join("; ")}`);
  return {
    row_count: rows.length,
    session_count: audit.session_count,
    primary_counts: counts,
    length_bands: lengthBands,
    input_word_range: { min: Number.isFinite(minInputWords) ? minInputWords : 0, max: maxInputWords },
    boundary_overlap_count: boundaryCount,
    over_40_input_words: over40Words,
    dense_context_count: denseContextCount,
    denied_source_count: deniedManifest.source_files.length,
    distribution_metrics: distributions,
  };
}

const smoke = process.argv.includes("--smoke");
const allRows = buildRows();
const rows = smoke ? allRows.slice(0, 2) : allRows;
const corpusText = `${rows.map((row) => JSON.stringify(row)).join("\n")}\n`;
const corpusHash = sha256(corpusText);
const audit = buildAssemblyAudit(rows, corpusHash);
const result = validate(rows, audit, { smoke });
if (smoke) {
  process.stdout.write(`${JSON.stringify({ mode: "smoke", status: "pass", ...result })}\n`);
  process.exit(0);
}

fs.mkdirSync(corpusDir, { recursive: true });
writeAtomic(corpusFile, corpusText);
const auditText = `${JSON.stringify(audit, null, 2)}\n`;
writeAtomic(auditFile, auditText);
const summary = {
  schema: "heardright.l1.synthetic-corpus-validation.v2",
  status: "pass",
  lifecycle: "candidate",
  split: "adjudication_candidate",
  training_eligible: false,
  corpus_file: path.basename(corpusFile),
  corpus_sha256: corpusHash,
  assembly_audit_file: path.basename(auditFile),
  assembly_audit_sha256: sha256(auditText),
  denied_manifest_file: path.basename(deniedFile),
  denied_manifest_sha256: deniedManifestHash,
  denied_source_count: deniedManifest.source_files.length,
  generator_file: path.relative(path.resolve(scriptDir, "../.."), scriptFile).replaceAll("\\", "/"),
  generator_sha256: generatorHash,
  template_catalog_sha256: templateCatalogHash,
  source_kind: "synthetic_authored_text",
  adjudication_status: "pending",
  ...result,
  checks: {
    schema_lifecycle_and_provenance: "pass",
    exact_primary_and_length_distribution: "pass",
    unique_rows_and_logical_clusters: "pass",
    denied_entity_phrase_and_near_match: "pass",
    context_structural_caps: "pass",
    natural_multi_sentence_domains_registers_and_discourse: "pass",
    document_frequency_and_structural_concentration_caps: "pass",
    permitted_edit_replay_and_structured_spans: "pass",
    protected_span_tail_deletion_and_loop_guards: "pass",
    actual_boundary_overlap_and_session_assembly: "pass",
    assembly_targets_absent_from_rows: "pass",
    generator_denied_manifest_and_audit_hashes: "pass"
  }
};
writeAtomic(summaryFile, `${JSON.stringify(summary, null, 2)}\n`);
process.stdout.write(`${JSON.stringify(summary)}\n`);
