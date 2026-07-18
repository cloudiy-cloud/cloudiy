// Quickstart: an AI agent rents GPU compute on Cloudiy.
//
// The whole arc in one file — discover a provider, let an LLM decide to buy
// compute, settle the x402 quote, and refuse the result unless the provider
// signed it:
//
//   1. discover  — inspect the provider: GPU, price, escrow program
//   2. decide    — Claude gets asToolSchema() and calls the tool itself
//   3. pay       — the node quotes in USDC (402); the agent settles and retries
//   4. verify    — the ed25519 result signature is checked before the agent
//                  is allowed to act on the output (this is the point)
//
// Runs with or without an Anthropic API key:
//
//   export ANTHROPIC_API_KEY=sk-ant-...        # real function-calling loop
//   node agent_rents_compute.mjs               # without the key: scripted mock
//
// The mock drives the identical Cloudiy path — only the "which tool should I
// call" decision is faked. `@anthropic-ai/sdk` is needed only for the real
// loop; the SDK itself stays zero-dependency.
//
// Prereq: a node on 127.0.0.1:8080 (`cloudiy share`), or pass one as argv[2].
import { CloudiyClient, PaymentRequiredError, SignatureError, asToolSchema } from "../cloudiy.mjs";

const NODE = process.argv[2] ?? "127.0.0.1:8080";
const MODEL = "claude-sonnet-5";

const client = new CloudiyClient(NODE);

// --- 1. discover ----------------------------------------------------------
// The HTTP SDK talks to one node you already know. To find nodes across the
// network, use the CLI (`cloudiy providers --via <directory>`) or the MCP tool
// `cloudiy_list_providers` — discovery rides the P2P/iroh transport, not HTTP.
async function discover() {
  const info = await client.info();
  console.log(
    `provider ${info.endpoint_id.slice(0, 16)}… — ${info.gpu_model}, ` +
      `${info.vram_mb} MB VRAM, ${info.price_usdc} USDC/job\n` +
      `  escrow ${info.escrow_program.slice(0, 16)}… on ${info.network}`,
  );
  return info;
}

// --- 2+3. the tool the agent calls ---------------------------------------
/** Run a kernel on a decentralized GPU, paying the x402 quote if asked.
 * The signature check is NOT optional: unsigned or tampered output throws
 * before the agent ever sees it. */
async function cloudiyGpuRun({ kernel, data }) {
  let result;
  try {
    result = await client.submit({ kernel, data });
  } catch (e) {
    if (!(e instanceof PaymentRequiredError)) throw e;
    // The node wants USDC. A real agent settles the Cloudiy escrow on Solana
    // here; demoPayment() stands in for that on devnet.
    console.log(`  402 → paying ${e.priceUsdc} USDC to ${e.payTo.slice(0, 16)}…`);
    result = await client.submit({ kernel, data, payment: e.demoPayment() });
  }

  // --- 4. verify (done by default inside submit) ---
  // Reaching this line means the ed25519 signature over
  // (job_id, sha256(input), sha256(output)) verified against the node's key.
  console.log(`  signature verified — signed by ${result.signedBy.slice(0, 16)}…`);
  return result.outputText;
}

const TOOLS = [asToolSchema(NODE)];

function dispatch(name, args) {
  if (name === "cloudiy_gpu_run") return cloudiyGpuRun(args);
  throw new Error(`unknown tool: ${name}`);
}

// --- the agent loop -------------------------------------------------------
const TASK =
  "Add the vectors [1,2,3] and [10,20,30] using decentralized GPU compute, " +
  "then tell me the result.";

async function runWithClaude() {
  const { default: Anthropic } = await import("@anthropic-ai/sdk");
  const llm = new Anthropic();
  const messages = [{ role: "user", content: TASK }];

  let response = await llm.messages.create({
    model: MODEL, max_tokens: 16000, tools: TOOLS, messages,
  });

  while (response.stop_reason === "tool_use") {
    messages.push({ role: "assistant", content: response.content });
    const results = [];
    for (const block of response.content) {
      if (block.type === "tool_use") {
        console.log(`agent calls ${block.name}(${JSON.stringify(block.input)})`);
        results.push({
          type: "tool_result",
          tool_use_id: block.id,
          content: await dispatch(block.name, block.input),
        });
      }
    }
    messages.push({ role: "user", content: results });
    response = await llm.messages.create({
      model: MODEL, max_tokens: 16000, tools: TOOLS, messages,
    });
  }

  for (const block of response.content) {
    if (block.type === "text") console.log(`\nagent: ${block.text}`);
  }
}

async function runMock() {
  console.log("(mock mode — no ANTHROPIC_API_KEY; the tool call below is scripted)\n");
  console.log(
    `tool schema handed to the LLM: ${TOOLS[0].name} ` +
      `[${Object.keys(TOOLS[0].input_schema.properties).join(", ")}]`,
  );
  const args = { kernel: "vector_add", data: "1,2,3;10,20,30" };
  console.log(`agent calls cloudiy_gpu_run(${JSON.stringify(args)})`);
  const output = await dispatch("cloudiy_gpu_run", args);
  console.log(`\nagent: The result is ${output.trim()}.`);
}

await discover();
console.log();
try {
  if (process.env.ANTHROPIC_API_KEY) await runWithClaude();
  else await runMock();
} catch (e) {
  if (e instanceof SignatureError) {
    // The whole point: an agent must not act on unverified compute.
    console.error(`\nREFUSED — ${e.message}`);
    process.exit(1);
  }
  throw e;
}
