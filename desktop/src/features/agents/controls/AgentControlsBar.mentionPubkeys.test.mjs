/**
 * AgentControlsBar dispatchSteer mentionPubkeys test (Defect 6 fix).
 *
 * Verifies that dispatchSteer passes mentionPubkeys=[agentPubkey] to
 * sendChannelMessage so the sidecar's mention filter admits the steer
 * message. Without this, the sidecar drops the message and the steer
 * command is pended indefinitely with no ack.
 *
 * This is a structural test — it reads the source file and asserts the
 * mentionPubkeys pattern is present alongside sendChannelMessage calls.
 */

import assert from "node:assert/strict";
import { describe, it } from "node:test";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const agentControlsBarPath = path.join(__dirname, "AgentControlsBar.tsx");
const source = fs.readFileSync(agentControlsBarPath, "utf8");

describe("AgentControlsBar dispatchSteer mentionPubkeys (Defect 6)", () => {
  it("sendChannelMessage in dispatchSteer passes mentionPubkeys", () => {
    // The dispatchSteer function must call sendChannelMessage with
    // mentionPubkeys including the agentPubkey.
    assert.ok(
      source.includes("mentionPubkeys"),
      "AgentControlsBar.tsx must reference mentionPubkeys",
    );

    // Verify the dispatchSteer function area contains the mentionPubkeys
    // pattern: [agentPubkey] passed as the 5th positional argument.
    const steerFnStart = source.indexOf("async function dispatchSteer()");
    assert.ok(steerFnStart > 0, "dispatchSteer function must exist");

    // Narrow to the dispatchSteer function body (everything from the
    // opening brace to the end of the function).
    const braceOpen = source.indexOf("{", steerFnStart);
    const steerFnBody = source.slice(braceOpen);

    assert.ok(
      steerFnBody.includes("mentionPubkeys"),
      "dispatchSteer body must reference mentionPubkeys",
    );
    assert.ok(
      steerFnBody.includes("[agentPubkey]"),
      "dispatchSteer must pass [agentPubkey] as mentionPubkeys value",
    );

    // Verify the call structure: find sendChannelMessage, then verify
    // both mentionPubkeys and [agentPubkey] appear inside the call
    // arguments (between the opening and closing parentheses).
    const sendIdx = steerFnBody.indexOf("sendChannelMessage(");
    assert.ok(sendIdx > 0, "sendChannelMessage call must exist in dispatchSteer");

    // Find the first semicolon after the sendChannelMessage open paren
    // — this is the end of the call statement.
    const openParen = steerFnBody.indexOf("(", sendIdx);
    let depth = 0;
    let closeParen = -1;
    for (let i = openParen; i < steerFnBody.length; i++) {
      if (steerFnBody[i] === "(") depth++;
      else if (steerFnBody[i] === ")") {
        depth--;
        if (depth === 0) {
          closeParen = i;
          break;
        }
      }
    }
    const callBlock = steerFnBody.slice(openParen, closeParen + 1);

    assert.ok(
      callBlock.includes("mentionPubkeys"),
      "sendChannelMessage call must include mentionPubkeys in arguments",
    );
    assert.ok(
      callBlock.includes("[agentPubkey]"),
      "sendChannelMessage call must reference [agentPubkey] in arguments",
    );
  });
});