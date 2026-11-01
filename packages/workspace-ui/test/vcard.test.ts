import assert from "node:assert/strict";
import test from "node:test";
import { buildVcf, parseVcf, parseContactsCsv } from "../src/lib/vcard.ts";

test("vCard round-trips name, emails, phones and org", () => {
  const input = [
    { name: "Ada Lovelace", emails: ["ada@x.com", "ada2@x.com"], phones: ["+15551234"], org: "Analytical" },
  ];
  const vcf = buildVcf(input);
  assert.match(vcf, /BEGIN:VCARD/);
  assert.match(vcf, /FN:Ada Lovelace/);
  const out = parseVcf(vcf);
  assert.equal(out.length, 1);
  assert.equal(out[0].name, "Ada Lovelace");
  assert.deepEqual(out[0].emails, ["ada@x.com", "ada2@x.com"]);
  assert.deepEqual(out[0].phones, ["+15551234"]);
  assert.equal(out[0].org, "Analytical");
});

test("parseVcf tolerates TYPE params and N fallback", () => {
  const vcf =
    "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Hopper;Grace;;;\r\nEMAIL;TYPE=WORK:grace@navy.mil\r\nEND:VCARD\r\n";
  const out = parseVcf(vcf);
  assert.equal(out[0].name, "Grace Hopper");
  assert.deepEqual(out[0].emails, ["grace@navy.mil"]);
});

test("parseVcf parses multiple cards", () => {
  const out = parseVcf(buildVcf([
    { name: "A", emails: ["a@x.com"], phones: [], org: "" },
    { name: "B", emails: ["b@x.com"], phones: [], org: "" },
  ]));
  assert.equal(out.length, 2);
});

test("parseContactsCsv reads name/email columns", () => {
  const csv = "Name,E-mail Address,Phone\r\nAda,ada@x.com,123\r\nGrace,grace@y.com,";
  const out = parseContactsCsv(csv);
  assert.equal(out.length, 2);
  assert.equal(out[0].name, "Ada");
  assert.deepEqual(out[0].emails, ["ada@x.com"]);
  assert.deepEqual(out[1].emails, ["grace@y.com"]);
});
