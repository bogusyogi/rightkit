import test from 'node:test';
import assert from 'node:assert/strict';
import { RESULT, reportStatus } from './types.mjs';
test('report status is typed & conservative', () => { assert.equal(reportStatus([{ status: RESULT.PASS }]), RESULT.PASS); assert.equal(reportStatus([{ status: RESULT.SKIPPED }]), RESULT.BLOCKED); assert.equal(reportStatus([{ status: RESULT.FAIL }]), RESULT.FAIL); });
