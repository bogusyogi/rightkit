import test from "node:test";
import assert from "node:assert/strict";
import { resolveComposeSignature } from "../src/store/actions/compose";

const baseState = {
  activeAccount: "a@example.com",
  signature: "Sig A",
  accountSigs: {},
  aliasSigs: {},
};

test("resolveComposeSignature loads the compose account signature when the active account differs", async () => {
  let requestedAccount = "";

  const signature = await resolveComposeSignature(
    baseState,
    "b@example.com",
    "b@example.com",
    async (accountId) => {
      requestedAccount = accountId;
      return "Sig B";
    },
  );

  assert.equal(requestedAccount, "b@example.com");
  assert.equal(signature, "Sig B");
});

test("resolveComposeSignature keeps alias signatures ahead of account fallbacks", async () => {
  const signature = await resolveComposeSignature(
    {
      ...baseState,
      accountSigs: { "b@example.com": "Sig B" },
      aliasSigs: { "alias@example.com": "Alias Sig" },
    },
    "alias@example.com",
    "b@example.com",
    async () => {
      throw new Error("cached alias signatures should not fetch");
    },
  );

  assert.equal(signature, "Alias Sig");
});
