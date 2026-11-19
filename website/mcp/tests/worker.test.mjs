import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { Script } from "node:vm";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { unstable_dev } from "wrangler";

let worker;
let client;
const catalog = JSON.parse(await readFile(new URL("../content/catalog.json", import.meta.url), "utf8"));

before(async () => {
  worker = await unstable_dev("src/index.ts", {
    local: true,
    ip: "127.0.0.1",
    port: 0,
    inspectorPort: 0,
    logLevel: "error",
    experimental: { disableExperimentalWarning: true },
  });
  client = new Client({ name: "regressify-package-test", version: "1.0.0" });
  await client.connect(new StreamableHTTPClientTransport(new URL(`http://${worker.address}:${worker.port}/mcp`)));
}, { timeout: 60000 });

after(async () => {
  try {
    await client?.close();
  } finally {
    await worker?.stop();
  }
});

test("MCP dependencies are exactly pinned and match the lockfile", async () => {
  const manifest = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  const lock = JSON.parse(await readFile(new URL("../package-lock.json", import.meta.url), "utf8"));
  for (const section of ["dependencies", "devDependencies"]) {
    assert.deepEqual(manifest[section], lock.packages[""][section]);
    for (const [name, version] of Object.entries(manifest[section])) {
      assert.match(version, /^\d+(?:\.\d+)*$/, `${name} must use an exact version`);
      assert.equal(lock.packages[`node_modules/${name}`].version, version);
    }
  }
});

test("MCP clients discover exactly the read-only authoring tools", async () => {
  const { tools } = await client.listTools();
  assert.deepEqual(tools.map((tool) => tool.name).sort(), ["get_authoring_guide", "get_example", "get_schema"]);
  for (const tool of tools) {
    assert.equal(tool.annotations.readOnlyHint, true);
    assert.equal(tool.annotations.destructiveHint, false);
  }
  assert.equal(client.getServerVersion().version, catalog.regressifyVersion);
});

test("every guide, schema, and example can be retrieved", async () => {
  for (const topic of Object.keys(catalog.guides)) {
    const result = await client.callTool({ name: "get_authoring_guide", arguments: { topic } });
    assert.equal(result.isError, undefined);
    assert.ok(result.content[0].text.includes(catalog.guides[topic].content));
    assert.ok(result.content[0].text.includes(catalog.regressifyVersion));
  }
  for (const kind of Object.keys(catalog.schemas)) {
    const result = await client.callTool({ name: "get_schema", arguments: { kind } });
    assert.equal(result.isError, undefined);
    assert.ok(result.content[0].text.includes('"$schema"'));
  }
  for (const action of catalog.actions) {
    const result = await client.callTool({ name: "get_schema", arguments: { kind: "action", action } });
    const schema = JSON.parse(result.content[0].text.split("\n\n").slice(1).join("\n\n"));
    assert.ok(schema.required.includes(action));
    assert.ok(schema.$defs);
    if (action === "key") {
      assert.deepEqual(schema.required, ["key"]);
      assert.equal(Object.hasOwn(schema.properties, "press"), false);
    }
    if (action === "press") {
      assert.ok(schema.required.includes("press") && schema.required.includes("key"));
    }
  }
  for (const task of Object.keys(catalog.examples)) {
    const result = await client.callTool({ name: "get_example", arguments: { task } });
    assert.equal(result.isError, undefined);
    assert.ok(result.content[0].text.includes(catalog.examples[task].path));
    assert.ok(result.content[0].text.includes(catalog.examples[task].content.trimEnd()), `Missing example body: ${task}`);
  }
});

test("startup, default guide, resources, and prompts deliver the supplied catalog", async () => {
  // Website-only deploys run this Worker against the deployed release, not necessarily HEAD.
  const { instructions, guides } = catalog;
  assert.equal(client.getInstructions(), instructions);
  const guide = await client.callTool({ name: "get_authoring_guide", arguments: {} });
  assert.equal(guide.isError, undefined);
  assert.ok(guide.content[0].text.includes(guides.overview.content));

  const { resources } = await client.listResources();
  assert.equal(resources.length, Object.keys(catalog.guides).length + Object.keys(catalog.schemas).length);
  for (const [topic, { content }] of Object.entries(guides)) {
    const resource = resources.find(({ name }) => name === `guide-${topic}`);
    assert.ok(resource, `Missing guide resource: ${topic}`);
    const result = await client.readResource({ uri: resource.uri });
    assert.ok(result.contents[0].text.includes(content), `Wrong guide resource: ${topic}`);
  }
  for (const [kind, schema] of Object.entries(catalog.schemas)) {
    const resource = resources.find(({ name }) => name === `schema-${kind}`);
    assert.ok(resource, `Missing schema resource: ${kind}`);
    const result = await client.readResource({ uri: resource.uri });
    assert.deepEqual(JSON.parse(result.contents[0].text), schema);
  }
  const { prompts } = await client.listPrompts();
  assert.equal(prompts[0].name, "author_regressify");
  for (const [topic, { content }] of Object.entries(guides)) {
    const prompt = await client.getPrompt({ name: "author_regressify", arguments: { topic } });
    const text = prompt.messages[0].content.text;
    assert.ok(text.includes(instructions), `Missing startup instructions in ${topic} prompt`);
    assert.ok(text.includes(guides.overview.content), `Missing overview in ${topic} prompt`);
    assert.ok(text.includes(content), `Missing selected guide in ${topic} prompt`);
  }
});

test("the guide tool advertises diagnostics and recipes", async () => {
  const { tools } = await client.listTools();
  const guide = tools.find((tool) => tool.name === "get_authoring_guide");
  assert.match(guide.description, /diagnos/i);
  assert.match(guide.description, /recipes/);
});

test("exported script examples are standard browser JavaScript", async () => {
  const scripts = {};
  for (const name of ["ready-script", "before-script", "scenario-ready-script"]) {
    const example = catalog.examples[name];
    assert.ok(example.path.endsWith(".js"));
    scripts[name] = new Script(example.content);
  }
  for (const isReference of [true, false]) {
    let removed = 0;
    const document = {
      documentElement: { dataset: {} },
      querySelector(selector) {
        assert.equal(selector, "#cookie-banner");
        return { remove() { removed++; } };
      },
    };
    const window = { regressify: { isReference } };
    for (const script of Object.values(scripts)) {
      await script.runInNewContext({ document, window });
    }
    assert.equal(document.documentElement.dataset.regressifyReady, "true");
    assert.equal(window.visualCaptureMode, isReference ? "reference" : "test");
    assert.equal(removed, 1);
    document.querySelector = () => null;
    await scripts["scenario-ready-script"].runInNewContext({ document, window });
  }
});

test("unknown topics and incompatible arguments fail without filesystem access", async () => {
  const unknown = await client.callTool({ name: "get_authoring_guide", arguments: { topic: "../../private" } });
  assert.equal(unknown.isError, true);
  const result = await client.callTool({ name: "get_schema", arguments: { kind: "workspace", action: "click" } });
  assert.equal(result.isError, true);
  const response = await worker.fetch("/content/catalog.json");
  assert.equal(response.status, 404);
});

test("deployment bundle is one minified module without source maps", { timeout: 120000 }, async () => {
  const outdir = await mkdtemp(join(tmpdir(), "regressify-mcp-bundle-"));
  try {
    const wrangler = fileURLToPath(new URL("../node_modules/wrangler/bin/wrangler.js", import.meta.url));
    await promisify(execFile)(process.execPath, [wrangler, "deploy", "--dry-run", "--outdir", outdir], {
      cwd: fileURLToPath(new URL("..", import.meta.url)),
      env: { ...process.env, WRANGLER_SEND_METRICS: "false" },
    });

    // Wrangler always writes a README note to --outdir; it is never uploaded.
    const files = (await readdir(outdir, { recursive: true })).filter((file) => file !== "README.md");
    assert.deepEqual(files, ["index.js"]);
    const bundle = await readFile(join(outdir, "index.js"), "utf8");
    assert.ok(!bundle.includes("sourceMappingURL"));
    assert.ok(!bundle.includes("// src/index.ts"), "esbuild module comments show an unminified bundle");
  } finally {
    await rm(outdir, { recursive: true, force: true });
  }
});

test("untrusted origins and oversized requests are rejected", async () => {
  const hostile = await worker.fetch("/mcp", {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream", Origin: "https://untrusted.invalid" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
  });
  assert.equal(hostile.status, 403);
  const oversized = await worker.fetch("/mcp", { method: "POST", body: "x".repeat(16385) });
  assert.equal(oversized.status, 413);
  const malformed = await worker.fetch("/mcp", {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream" },
    body: "{",
  });
  assert.ok(malformed.status >= 400);
});

test("the stateless protocol supports discovery and tool requests", async () => {
  const metadata = {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientCapabilities": {},
    "io.modelcontextprotocol/clientInfo": { name: "regressify-stateless-test", version: "1.0.0" },
  };
  for (const method of ["server/discover", "tools/list"]) {
    const response = await worker.fetch("/mcp", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/json, text/event-stream",
        "MCP-Protocol-Version": "2026-07-28",
        "Mcp-Method": method,
      },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params: { _meta: metadata } }),
    });
    assert.equal(response.status, 200, await response.clone().text());
    const result = await response.json();
    assert.equal(result.error, undefined);
    assert.ok(result.result);
  }
});