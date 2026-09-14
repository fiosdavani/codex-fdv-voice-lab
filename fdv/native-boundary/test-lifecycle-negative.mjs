// Positive harness control: runner must surface this deliberate failure.
import assert from 'node:assert/strict';
import test from 'node:test';
test('INTENTIONAL_LIFECYCLE_NEGATIVE_CONTROL', () => {
  assert.equal(process.env.FDV_LIFECYCLE_NEGATIVE, undefined, 'INTENTIONAL_LIFECYCLE_NEGATIVE_CONTROL');
});
