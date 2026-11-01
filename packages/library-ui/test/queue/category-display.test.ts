import assert from "node:assert/strict";
import { test } from "node:test";
import { FileText, Globe, Users, Video } from "lucide-react";
import { CATEGORY_ICON, categoryIcon } from "../../src/queue/components/CategoryIcon";
import { categoryToneClass, SOCIAL_HEX } from "../../src/queue/lib/categoryDisplay";

test("each category maps to one icon; other has none", () => {
  assert.equal(categoryIcon("media"), Video);
  assert.equal(categoryIcon("social"), Users);
  assert.equal(categoryIcon("document"), FileText);
  assert.equal(categoryIcon("web"), Globe);
  assert.equal(categoryIcon("other"), null);
  assert.equal(categoryIcon("unknown-category"), null);
  assert.equal(CATEGORY_ICON.other, null);
});

test("social tone uses the #a993d8 purple and not the retired #a78bfa", () => {
  assert.equal(SOCIAL_HEX, "#a993d8");
  const tone = categoryToneClass("social");
  assert.match(tone, /#a993d8/);
  assert.doesNotMatch(tone, /#a78bfa/i);
});

test("unknown categories get no tone and every known category has one", () => {
  assert.equal(categoryToneClass("nope"), "");
  for (const cat of ["media", "social", "document", "web", "other"]) {
    assert.notEqual(categoryToneClass(cat), "", cat);
  }
});
