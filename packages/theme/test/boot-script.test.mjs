import { test } from "node:test";
import assert from "node:assert/strict";
import { themeBootScript, parseBrand } from "../dist/index.js";
import { isFontFamilyList } from "../dist/wordmark.js";

// Golden script text produced by the HEAD copies of `@rightkit/theme`'s and `@rightkit/react-shell`'s
// `themeBootScript` before they were merged. Any change here changes every app's CSP hash.
const GOLDEN = {
  themeNoOpts: "(function(){try{var k=\"k.theme\",c=\"system\",p=true,d=false;try{var v=localStorage.getItem(k);if(v===\"light\"||v===\"dark\"||v===\"system\")c=v}catch(_){}var r=document.documentElement;if(c===\"system\"){if(!p){r.removeAttribute(\"data-theme\");return}c=matchMedia(\"(prefers-color-scheme: dark)\").matches?\"dark\":\"light\"}if(c===\"dark\"&&d){r.removeAttribute(\"data-theme\");return}r.setAttribute(\"data-theme\",c)}catch(_){}})();",
  themeOpts: "(function(){try{var k=\"k.theme\",c=\"dark\",p=false,d=true;try{var v=localStorage.getItem(k);if(v===\"light\"||v===\"dark\"||v===\"system\")c=v}catch(_){}var r=document.documentElement;if(c===\"system\"){if(!p){r.removeAttribute(\"data-theme\");return}c=matchMedia(\"(prefers-color-scheme: dark)\").matches?\"dark\":\"light\"}if(c===\"dark\"&&d){r.removeAttribute(\"data-theme\");return}r.setAttribute(\"data-theme\",c)}catch(_){}})();",
  shellQaDark: "(function(){try{var k=\"k\",c=\"light\",p=false,d=true,q=\"dark\";if(q){c=q}else{try{var v=localStorage.getItem(k);if(v===\"light\"||v===\"dark\"||v===\"system\")c=v}catch(_){}}var r=document.documentElement;if(c===\"system\"){if(!p){r.removeAttribute(\"data-theme\");return}c=matchMedia(\"(prefers-color-scheme: dark)\").matches?\"dark\":\"light\"}if(c===\"dark\"&&d){r.removeAttribute(\"data-theme\");return}r.setAttribute(\"data-theme\",c)}catch(_){}})();",
  shellNoQa: "(function(){try{var k=\"x\",c=\"system\",p=true,d=false,q=null;if(q){c=q}else{try{var v=localStorage.getItem(k);if(v===\"light\"||v===\"dark\"||v===\"system\")c=v}catch(_){}}var r=document.documentElement;if(c===\"system\"){if(!p){r.removeAttribute(\"data-theme\");return}c=matchMedia(\"(prefers-color-scheme: dark)\").matches?\"dark\":\"light\"}if(c===\"dark\"&&d){r.removeAttribute(\"data-theme\");return}r.setAttribute(\"data-theme\",c)}catch(_){}})();",
  shellQaSystem: "(function(){try{var k=\"x\",c=\"system\",p=true,d=false,q=null;if(q){c=q}else{try{var v=localStorage.getItem(k);if(v===\"light\"||v===\"dark\"||v===\"system\")c=v}catch(_){}}var r=document.documentElement;if(c===\"system\"){if(!p){r.removeAttribute(\"data-theme\");return}c=matchMedia(\"(prefers-color-scheme: dark)\").matches?\"dark\":\"light\"}if(c===\"dark\"&&d){r.removeAttribute(\"data-theme\");return}r.setAttribute(\"data-theme\",c)}catch(_){}})();",
};

test("themeBootScript without qaOverride is byte-identical to the old theme script", () => {
  assert.equal(themeBootScript("k.theme"), GOLDEN.themeNoOpts);
  assert.equal(
    themeBootScript("k.theme", { defaultChoice: "dark", alwaysPaintDataTheme: false, darkRemovesAttribute: true }),
    GOLDEN.themeOpts,
  );
});

test("themeBootScript with qaOverride is byte-identical to the old react-shell script", () => {
  assert.equal(
    themeBootScript("k", {
      qaOverride: "dark",
      defaultChoice: "light",
      alwaysPaintDataTheme: false,
      darkRemovesAttribute: true,
    }),
    GOLDEN.shellQaDark,
  );
  assert.equal(themeBootScript("x", { qaOverride: null }), GOLDEN.shellNoQa);
  assert.equal(themeBootScript("x", { qaOverride: "system" }), GOLDEN.shellQaSystem);
});

test("a wordmark that is a font-family list is not a product name, but parseBrand still accepts it", () => {
  assert.equal(isFontFamilyList("Tanker, sans-serif"), true);
  assert.equal(isFontFamilyList("system-ui"), false);
  assert.equal(isFontFamilyList("CutRight"), false);
  assert.equal(isFontFamilyList("Acme, Inc."), false);
  assert.equal(isFontFamilyList("Monospace, Inc."), false);
  const brand = { name: "CutRight", colors: { brand: "#1f6feb" } };
  assert.doesNotThrow(() => parseBrand({ ...brand, wordmark: "Tanker, sans-serif" }));
  assert.doesNotThrow(() => parseBrand({ ...brand, wordmark: "CutRight", wordmarkFont: '"Tanker", sans-serif' }));
});
