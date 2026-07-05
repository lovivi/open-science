import { useCallback, useEffect, useState } from "react";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Download,
  ExternalLink,
  FolderOpen,
  Loader2,
  NotebookPen,
  Search,
} from "lucide-react";
import type {
  McpServer,
  OAuthAuthorization,
  ProviderAuthMethod,
  ProviderCatalogEntry,
  ProviderInfo,
} from "@ai4s/sdk";
import { useUiStore } from "@/lib/store";
import { getClient, useRuntimeStore } from "@/lib/runtime";
import {
  importOpenCodeLogin,
  isTauri,
  jupyterStatus,
  openExternal,
  openWorkspaceBase,
  pickFolder,
  removeConfigEntry,
  setupJupyter,
  setWorkspaceBase,
  startJupyter,
  workspaceBase,
  type JupyterStatus,
} from "@/lib/tauri";
import { setupScienceMcp } from "@/lib/tauri";
import { ClusterCard } from "@/components/settings/ClusterCard";
import { ModalCard } from "@/components/settings/ModalCard";
import { DataFlowCard } from "@/components/settings/DataFlowCard";
import { WslBackendCard } from "@/components/settings/WslBackendCard";
import { SCIENCE_CONNECTORS, connectorConfig } from "@/lib/scienceConnectors";
import { toast } from "@/lib/toast";
import { cn } from "@/lib/cn";

/**
 * Settings. ONE configuration surface: everything talks to the bundled
 * OpenCode's own config/auth API — no separate "model key" concept.
 */
export function SettingsPage() {
  const theme = useUiStore((s) => s.theme);
  const setTheme = useUiStore((s) => s.setTheme);
  const { status, serverUrl, setServerUrl, connect, disconnect, defaultModel, loadCatalog } =
    useRuntimeStore();
  const connected = status === "ready";

  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [authMethods, setAuthMethods] = useState<Record<string, ProviderAuthMethod[]>>({});
  const [catalog, setCatalog] = useState<ProviderCatalogEntry[]>([]);
  const [customIds, setCustomIds] = useState<string[]>([]);
  const [mcpServers, setMcpServers] = useState<McpServer[]>([]);
  const [jupyter, setJupyter] = useState<JupyterStatus | null>(null);
  const [settingUpJupyter, setSettingUpJupyter] = useState(false);
  // Which curated science connector is currently being provisioned, by id.
  const [enablingConnector, setEnablingConnector] = useState<string | null>(null);
  // API keys typed for key-requiring connectors, keyed by connector id.
  const [connectorKeys, setConnectorKeys] = useState<Record<string, string>>({});

  // Add-MCP-server form.
  const [mName, setMName] = useState("");
  const [mType, setMType] = useState<"local" | "remote">("local");
  const [mTarget, setMTarget] = useState("");
  const [wsPath, setWsPath] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Custom endpoint form (self-hosted / Ollama / OpenAI- or Anthropic-compatible).
  const [showCustom, setShowCustom] = useState(false);
  const [cName, setCName] = useState("");
  const [cNpm, setCNpm] = useState("@ai-sdk/openai-compatible");
  const [cUrl, setCUrl] = useState("");
  const [cKey, setCKey] = useState("");
  const [cModels, setCModels] = useState("");

  // Connect-a-provider flow state.
  const [connectQuery, setConnectQuery] = useState("");
  const [keyInput, setKeyInput] = useState("");
  const [promptInputs, setPromptInputs] = useState<Record<string, string>>({});
  const [oauth, setOauth] = useState<
    (OAuthAuthorization & { providerID: string; methodIndex: number }) | null
  >(null);
  const [codeInput, setCodeInput] = useState("");

  const refresh = useCallback(async () => {
    const client = getClient();
    if (!client) return;
    try {
      const [p, m, c, custom, mcp] = await Promise.all([
        client.listProviders(),
        client.listAuthMethods(),
        client.listProviderCatalog(),
        client.listCustomProviderIds(),
        client.listMcpServers().catch(() => []),
      ]);
      setProviders(p);
      setAuthMethods(m);
      setCatalog(c.all);
      setCustomIds(custom);
      setMcpServers(mcp);
      setJupyter(await jupyterStatus());
    } catch {
      /* runtime not ready yet */
    }
  }, []);

  useEffect(() => {
    if (connected) void refresh();
  }, [connected, refresh]);
  useEffect(() => {
    // The BASE folder — the parent every session's dated subfolder is created
    // under. (The per-session active folder shows in the conversation header.)
    void workspaceBase().then(setWsPath);
  }, []);

  const changeWorkspaceBase = async () => {
    const picked = await pickFolder();
    if (!picked) return;
    try {
      setWsPath(await setWorkspaceBase(picked));
      toast.success("New sessions will be created in this folder.");
    } catch (err) {
      toast.error(`Could not set the folder: ${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const run = async (label: string, fn: () => Promise<void>) => {
    setBusy(true);
    try {
      await fn();
      await refresh();
      await loadCatalog();
    } catch (e) {
      toast.error(`${label}: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const saveModel = (model: string) =>
    run("Could not set the model", async () => {
      if (model) await getClient()!.setDefaultModel(model);
      toast.success(`Default model set to ${model}`);
    });

  const saveKey = (providerID: string) =>
    run("Could not save the key", async () => {
      await getClient()!.setProviderApiKey(providerID, keyInput.trim());
      setKeyInput("");
      setConnectQuery("");
      toast.success(`${providerID} connected`);
    });

  const startOAuth = (providerID: string, methodIndex: number, inputs?: Record<string, string>) =>
    run("Could not start the login", async () => {
      const auth = await getClient()!.oauthAuthorize(providerID, methodIndex, inputs);
      setOauth({ ...auth, providerID, methodIndex });
      await openExternal(auth.url);
    });

  const completeOAuth = () =>
    run("Login did not complete", async () => {
      if (!oauth) return;
      await getClient()!.oauthCallback(
        oauth.providerID,
        oauth.methodIndex,
        codeInput.trim() || undefined,
      );
      toast.success(`${oauth.providerID} connected`);
      setOauth(null);
      setCodeInput("");
    });

  const disconnectProvider = (providerID: string) =>
    run("Could not remove", async () => {
      if (customIds.includes(providerID)) {
        // Custom endpoints live in the config file; removal restarts the sidecar.
        await removeConfigEntry("provider", providerID);
        await useRuntimeStore.getState().connectRetry();
      } else {
        await getClient()!.removeProviderAuth(providerID);
      }
      toast.success(`${providerID} removed`);
    });

  const saveCustom = () =>
    run("Could not add the endpoint", async () => {
      const id = cName.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
      const models = cModels.split(",").map((s) => s.trim()).filter(Boolean);
      if (!id || !cUrl.trim() || models.length === 0) {
        toast.error("Name, base URL and at least one model id are required.");
        return;
      }
      await getClient()!.addCustomProvider(id, {
        name: cName.trim(),
        npm: cNpm,
        baseURL: cUrl.trim(),
        apiKey: cKey.trim() || undefined,
        models,
      });
      toast.success(`${cName.trim()} added — its models are now selectable above.`);
      setShowCustom(false);
      setCName("");
      setCUrl("");
      setCKey("");
      setCModels("");
    });

  const addMcp = () =>
    run("Could not add the MCP server", async () => {
      const name = mName.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
      const target = mTarget.trim();
      if (!name || !target) {
        toast.error("Name and command/URL are required.");
        return;
      }
      await getClient()!.addMcpServer(
        name,
        mType === "local"
          ? { type: "local", command: target.split(/\s+/), enabled: true }
          : { type: "remote", url: target, enabled: true },
      );
      toast.success(`MCP server ${name} added`);
      setMName("");
      setMTarget("");
    });

  // One click: uv provisions the isolated Jupyter env, the app starts the
  // server, and the MCP entry (URL + token) is written into OpenCode's config.
  const enableJupyter = async () => {
    setSettingUpJupyter(true);
    try {
      toast.success("Setting up Jupyter — first run downloads a few hundred MB, please wait…");
      await setupJupyter();
      const s = await startJupyter();
      if (!s.url || !s.token || !s.mcp_command) throw new Error("setup finished incomplete");
      await getClient()!.addMcpServer("jupyter", {
        type: "local",
        command: [s.mcp_command],
        enabled: true,
        environment: { JUPYTER_URL: s.url, JUPYTER_TOKEN: s.token, ALLOW_IMG_OUTPUT: "true" },
      });
      toast.success("Jupyter MCP enabled — the agent can now drive notebooks.");
      await refresh();
      await loadCatalog();
    } catch (e) {
      toast.error(`Jupyter setup failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setSettingUpJupyter(false);
    }
  };

  // One click: uv provisions the open-source connector into the shared science
  // env, then its MCP entry is written into OpenCode's config.
  const enableConnector = async (id: string) => {
    const c = SCIENCE_CONNECTORS.find((x) => x.id === id);
    if (!c) return;
    setEnablingConnector(id);
    try {
      toast.success(`Setting up ${c.label} — first run downloads a managed Python, please wait…`);
      const python = await setupScienceMcp(c.pkg);
      await getClient()!.addMcpServer(c.id, connectorConfig(c, python, connectorKeys[c.id]));
      toast.success(`${c.label} enabled — the agent can now use it from chat.`);
      setConnectorKeys((k) => ({ ...k, [c.id]: "" })); // don't keep the key in UI state
      await refresh();
      await loadCatalog();
    } catch (e) {
      toast.error(`${c.label} setup failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setEnablingConnector(null);
    }
  };

  const removeMcp = (name: string) =>
    run("Could not remove the MCP server", async () => {
      await removeConfigEntry("mcp", name);
      await useRuntimeStore.getState().connectRetry();
      toast.success(`MCP server ${name} removed`);
    });

  const importLogin = () =>
    run("Import failed", async () => {
      const found = await importOpenCodeLogin();
      if (!found) {
        toast.error("No OpenCode CLI login found on this machine.");
        return;
      }
      // The sidecar restarted with the imported credentials — reconnect.
      await useRuntimeStore.getState().connectRetry();
      toast.success("Imported your OpenCode CLI login.");
    });

  // Resolve the search box to a catalog entry (by id or exact name).
  const q = connectQuery.trim().toLowerCase();
  const selected =
    catalog.find((p) => p.id === q) ?? catalog.find((p) => p.name.toLowerCase() === q) ?? null;
  // Every provider takes an API key via PUT /auth; special flows (OAuth) add to that.
  const methods: ProviderAuthMethod[] = selected
    ? [
        ...(authMethods[selected.id] ?? []).filter((m) => m.type === "oauth"),
        { type: "api", label: "API key" },
      ]
    : [];

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-2xl px-8 pb-16 pt-8">
        <h1 className="font-serif text-xl text-text">Settings</h1>
        <p className="mt-0.5 text-xs text-muted">
          Everything here configures the bundled OpenCode runtime — one config, no copies.
        </p>

        {/* ---- Agent runtime ---- */}
        <Card title="Agent runtime" hint="opencode serve, driven over its HTTP + SSE API">
          <div className="flex items-center gap-2">
            <input
              value={serverUrl}
              onChange={(e) => setServerUrl(e.target.value)}
              placeholder="http://127.0.0.1:4096"
              className={inputCls("flex-1 font-mono")}
            />
            {connected ? (
              <button onClick={disconnect} className={btnGhost()}>
                Disconnect
              </button>
            ) : (
              <button onClick={connect} className={btnAccent()}>
                Connect
              </button>
            )}
          </div>
          <div className="mt-2.5 flex items-center gap-1.5 text-xs text-muted">
            <span
              className={cn(
                "h-1.5 w-1.5 rounded-full",
                connected ? "bg-ok" : status === "error" ? "bg-error" : "bg-muted",
              )}
            />
            <span className="capitalize">{status}</span>
            {connected && defaultModel && (
              <>
                <span className="text-border">·</span>
                <span className="font-mono">{defaultModel}</span>
              </>
            )}
          </div>
        </Card>

        {/* ---- Models & providers ---- */}
        <Card title="Model" hint="Providers below supply the models you can pick here">
          {!connected ? (
            <p className="text-[13px] text-muted">Connect the runtime to configure models.</p>
          ) : (
            <>
              <div className="relative">
                <select
                  value={defaultModel ?? ""}
                  onChange={(e) => void saveModel(e.target.value)}
                  disabled={busy}
                  className={cn(inputCls("w-full appearance-none pr-9"), "cursor-pointer")}
                >
                  <option value="">Not set — pick a default model</option>
                  {providers.map((p) => (
                    <optgroup key={p.id} label={p.name}>
                      {p.models.map((m) => (
                        <option key={m.id} value={`${p.id}/${m.id}`}>
                          {m.name}
                        </option>
                      ))}
                    </optgroup>
                  ))}
                </select>
                <ChevronDown
                  size={14}
                  className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 text-muted"
                />
              </div>

              <Divider label="Providers" />

              <div className="overflow-hidden rounded-input border border-border">
                {providers.map((p, i) => (
                  <div
                    key={p.id}
                    className={cn(
                      "flex h-10 items-center gap-2.5 bg-surface px-3 text-[13px]",
                      i > 0 && "border-t border-border",
                    )}
                  >
                    <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-ok" />
                    <span className="font-medium text-text">{p.name}</span>
                    <span className="text-xs text-muted">
                      {p.models.length} model{p.models.length === 1 ? "" : "s"}
                    </span>
                    <div className="flex-1" />
                    {p.id === "opencode" ? (
                      <span className="rounded-full bg-surface-2 px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted ring-1 ring-border">
                        built-in · free
                      </span>
                    ) : (
                      <button
                        className="text-xs text-muted transition-colors hover:text-error"
                        onClick={() => void disconnectProvider(p.id)}
                        disabled={busy}
                        title="Remove this provider's credentials/config"
                      >
                        Remove
                      </button>
                    )}
                  </div>
                ))}

                {/* Connect a provider */}
                <div className="border-t border-border bg-surface-2/50 p-3">
                  <div className="relative">
                    <Search
                      size={13}
                      className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-muted"
                    />
                    <input
                      list="provider-catalog"
                      value={connectQuery}
                      onChange={(e) => {
                        setConnectQuery(e.target.value);
                        setOauth(null);
                        setPromptInputs({});
                      }}
                      placeholder={`Connect a provider — search ${catalog.length} (anthropic, openrouter, deepseek…)`}
                      className={inputCls("w-full pl-8")}
                    />
                    <datalist id="provider-catalog">
                      {catalog.map((p) => (
                        <option key={p.id} value={p.id}>
                          {p.name}
                        </option>
                      ))}
                    </datalist>
                  </div>

                  {selected && (
                    <div className="mt-2 space-y-2">
                      {methods.map((m, i) =>
                        m.type === "oauth" ? (
                          <div key={i} className="space-y-1.5">
                            {(m.prompts ?? []).map((pr) =>
                              pr.type === "select" ? (
                                <select
                                  key={pr.key}
                                  value={promptInputs[pr.key] ?? ""}
                                  onChange={(e) =>
                                    setPromptInputs((s) => ({ ...s, [pr.key]: e.target.value }))
                                  }
                                  className={inputCls("w-full")}
                                >
                                  <option value="">{pr.message}</option>
                                  {(pr.options ?? []).map((o) => (
                                    <option key={o.value} value={o.value}>
                                      {o.label}
                                      {o.hint ? ` — ${o.hint}` : ""}
                                    </option>
                                  ))}
                                </select>
                              ) : (
                                <input
                                  key={pr.key}
                                  value={promptInputs[pr.key] ?? ""}
                                  onChange={(e) =>
                                    setPromptInputs((s) => ({ ...s, [pr.key]: e.target.value }))
                                  }
                                  placeholder={pr.message}
                                  className={inputCls("w-full")}
                                />
                              ),
                            )}
                            <button
                              className={btnGhost("gap-1.5")}
                              onClick={() => void startOAuth(selected.id, i, promptInputs)}
                              disabled={busy}
                            >
                              <ExternalLink size={12} /> {m.label}
                            </button>
                          </div>
                        ) : null,
                      )}

                      <div className="flex items-center gap-2">
                        <input
                          type="password"
                          value={keyInput}
                          onChange={(e) => setKeyInput(e.target.value)}
                          placeholder={`${selected.name} API key${selected.env[0] ? ` (${selected.env[0]})` : ""}`}
                          className={inputCls("flex-1 font-mono")}
                        />
                        <button
                          className={btnAccent()}
                          onClick={() => void saveKey(selected.id)}
                          disabled={busy || !keyInput.trim()}
                        >
                          <Check size={13} /> Save
                        </button>
                      </div>
                    </div>
                  )}

                  {oauth && (
                    <div className="mt-2 space-y-2 rounded-input border border-border bg-surface p-3">
                      <p className="text-xs leading-relaxed text-muted">{oauth.instructions}</p>
                      {oauth.method === "code" && (
                        <input
                          value={codeInput}
                          onChange={(e) => setCodeInput(e.target.value)}
                          placeholder="Paste the code from the browser"
                          className={inputCls("w-full font-mono")}
                        />
                      )}
                      <button
                        className={btnAccent()}
                        onClick={() => void completeOAuth()}
                        disabled={busy || (oauth.method === "code" && !codeInput.trim())}
                      >
                        {busy ? <Loader2 size={12} className="animate-spin" /> : <Check size={13} />}
                        Complete login
                      </button>
                    </div>
                  )}
                </div>

                {/* Custom endpoint */}
                <div className="border-t border-border">
                  <button
                    className="flex h-10 w-full items-center gap-2 px-3 text-left text-[13px] text-muted transition-colors hover:text-text"
                    onClick={() => setShowCustom((s) => !s)}
                    aria-expanded={showCustom}
                  >
                    <ChevronRight
                      size={13}
                      className={cn("transition-transform", showCustom && "rotate-90")}
                    />
                    Custom endpoint
                    <span className="text-xs text-muted/70">
                      self-hosted · local Ollama · OpenAI/Anthropic-compatible
                    </span>
                  </button>
                  {showCustom && (
                    <div className="space-y-2 px-3 pb-3">
                      <div className="flex gap-2">
                        <input
                          value={cName}
                          onChange={(e) => setCName(e.target.value)}
                          placeholder="Name — e.g. Ollama, My DeepSeek gateway"
                          className={inputCls("flex-1")}
                        />
                        <select
                          value={cNpm}
                          onChange={(e) => setCNpm(e.target.value)}
                          className={inputCls("w-[190px]")}
                        >
                          <option value="@ai-sdk/openai-compatible">OpenAI-compatible</option>
                          <option value="@ai-sdk/anthropic">Anthropic-compatible</option>
                        </select>
                      </div>
                      <input
                        value={cUrl}
                        onChange={(e) => setCUrl(e.target.value)}
                        placeholder="Base URL — Ollama: http://127.0.0.1:11434/v1"
                        className={inputCls("w-full font-mono")}
                      />
                      <div className="flex gap-2">
                        <input
                          type="password"
                          value={cKey}
                          onChange={(e) => setCKey(e.target.value)}
                          placeholder="API key — optional, Ollama needs none"
                          className={inputCls("flex-1 font-mono")}
                        />
                        <input
                          value={cModels}
                          onChange={(e) => setCModels(e.target.value)}
                          placeholder="Model ids, comma-separated"
                          className={inputCls("flex-1 font-mono")}
                        />
                      </div>
                      <button className={btnAccent()} onClick={() => void saveCustom()} disabled={busy}>
                        Add endpoint
                      </button>
                    </div>
                  )}
                </div>
              </div>

              {isTauri && (
                <button
                  className="mt-3 flex items-center gap-1.5 text-xs text-muted transition-colors hover:text-text"
                  onClick={() => void importLogin()}
                  disabled={busy}
                >
                  <Download size={12} />
                  Already use the OpenCode CLI? Import its login
                </button>
              )}
            </>
          )}
        </Card>

        {/* ---- MCP servers ---- */}
        <Card
          title="MCP servers"
          hint="Extra tools for the agent (Model Context Protocol) — e.g. a Jupyter or browser MCP"
        >
          {!connected ? (
            <p className="text-[13px] text-muted">Connect the runtime to configure MCP servers.</p>
          ) : (
            <div className="overflow-hidden rounded-input border border-border">
              {/* Curated open-source science connectors — one-click enable. */}
              {isTauri &&
                SCIENCE_CONNECTORS.filter((c) => !mcpServers.some((s) => s.name === c.id)).map(
                  (c) => {
                    const keyMissing = Boolean(c.apiKeyEnv) && !connectorKeys[c.id]?.trim();
                    return (
                      <div
                        key={c.id}
                        className="border-b border-border bg-surface px-3 py-2.5 text-[13px]"
                      >
                        <div className="flex items-center gap-2.5">
                          <Search size={14} className="shrink-0 text-muted" />
                          <div className="min-w-0 flex-1">
                            <span className="font-medium text-text">{c.label}</span>
                            <span className="ml-2 rounded bg-surface-2 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted ring-1 ring-border">
                              {c.discipline}
                            </span>
                            <span className="ml-1.5 rounded bg-surface-2 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted ring-1 ring-border">
                              open source
                            </span>
                            <div className="truncate text-xs text-muted">{c.description}</div>
                            <div className="truncate font-mono text-[11px] text-muted/70">
                              {c.source}
                              {c.installNote ? ` · ${c.installNote}` : ""}
                            </div>
                          </div>
                          <button
                            className={btnAccent("h-8")}
                            onClick={() => void enableConnector(c.id)}
                            disabled={enablingConnector !== null || busy || keyMissing}
                            title={keyMissing ? "Enter the API key first" : undefined}
                          >
                            {enablingConnector === c.id ? (
                              <>
                                <Loader2 size={12} className="animate-spin" /> Setting up…
                              </>
                            ) : (
                              "Enable"
                            )}
                          </button>
                        </div>
                        {c.apiKeyEnv && (
                          <div className="mt-2 flex items-center gap-2 pl-6">
                            <input
                              type="password"
                              value={connectorKeys[c.id] ?? ""}
                              onChange={(e) =>
                                setConnectorKeys((k) => ({ ...k, [c.id]: e.target.value }))
                              }
                              placeholder={`${c.apiKeyEnv} (free key)`}
                              className="h-8 min-w-0 flex-1 rounded-input border border-border bg-surface-2 px-2 font-mono text-[12px] text-text placeholder:text-muted/60"
                            />
                            {c.apiKeyUrl && (
                              <a
                                href={c.apiKeyUrl}
                                target="_blank"
                                rel="noreferrer"
                                className="inline-flex items-center gap-1 whitespace-nowrap text-[11px] text-accent hover:underline"
                              >
                                <ExternalLink size={11} /> Get a free key
                              </a>
                            )}
                          </div>
                        )}
                      </div>
                    );
                  },
                )}
              {/* Featured: one-click Jupyter (shown until its MCP entry exists). */}
              {isTauri && !mcpServers.some((s) => s.name === "jupyter") && (
                <div className="flex items-center gap-2.5 border-b border-border bg-surface px-3 py-2.5 text-[13px]">
                  <NotebookPen size={14} className="shrink-0 text-muted" />
                  <div className="min-w-0 flex-1">
                    <span className="font-medium text-text">Jupyter</span>
                    <span className="ml-2 text-xs text-muted">
                      lets the agent drive real notebooks · isolated env, ~300 MB on first run
                    </span>
                  </div>
                  <button
                    className={btnAccent("h-8")}
                    onClick={() => void enableJupyter()}
                    disabled={settingUpJupyter || busy}
                  >
                    {settingUpJupyter ? (
                      <>
                        <Loader2 size={12} className="animate-spin" /> Setting up…
                      </>
                    ) : jupyter?.installed ? (
                      "Enable"
                    ) : (
                      "Set up & enable"
                    )}
                  </button>
                </div>
              )}
              {mcpServers.map((s, i) => (
                <div
                  key={s.name}
                  className={cn(
                    "flex h-10 items-center gap-2.5 bg-surface px-3 text-[13px]",
                    i > 0 && "border-t border-border",
                  )}
                >
                  <span
                    className={cn(
                      "h-1.5 w-1.5 shrink-0 rounded-full",
                      s.status === "connected"
                        ? "bg-ok"
                        : s.status === "failed"
                          ? "bg-error"
                          : "bg-muted",
                    )}
                  />
                  <span className="font-medium text-text">{s.name}</span>
                  <span className="text-xs text-muted">
                    {s.config?.type ?? "?"} · {s.status}
                  </span>
                  <span className="max-w-[260px] flex-1 truncate text-right font-mono text-[11px] text-muted/70">
                    {s.config?.type === "local"
                      ? s.config.command.join(" ")
                      : s.config?.type === "remote"
                        ? s.config.url
                        : ""}
                  </span>
                  <button
                    className="shrink-0 text-xs text-muted transition-colors hover:text-error"
                    onClick={() => void removeMcp(s.name)}
                    disabled={busy}
                  >
                    Remove
                  </button>
                </div>
              ))}

              <div
                className={cn(
                  "space-y-2 bg-surface-2/50 p-3",
                  mcpServers.length > 0 && "border-t border-border",
                )}
              >
                <div className="flex gap-2">
                  <input
                    value={mName}
                    onChange={(e) => setMName(e.target.value)}
                    placeholder="Name — e.g. jupyter, playwright"
                    className={inputCls("flex-1")}
                  />
                  <select
                    value={mType}
                    onChange={(e) => setMType(e.target.value as "local" | "remote")}
                    className={inputCls("w-[110px]")}
                  >
                    <option value="local">local</option>
                    <option value="remote">remote</option>
                  </select>
                </div>
                <div className="flex gap-2">
                  <input
                    value={mTarget}
                    onChange={(e) => setMTarget(e.target.value)}
                    placeholder={
                      mType === "local"
                        ? "Command — e.g. npx -y @playwright/mcp"
                        : "URL — e.g. https://example.com/mcp"
                    }
                    className={inputCls("flex-1 font-mono")}
                  />
                  <button className={btnAccent()} onClick={() => void addMcp()} disabled={busy}>
                    Add server
                  </button>
                </div>
              </div>
            </div>
          )}
        </Card>

        {/* ---- Workspace ---- */}
        <Card
          title="Workspace"
          hint="Local-first — each session works in its own dated subfolder created here"
        >
          <div className="flex items-center gap-2">
            <span
              className={cn(
                inputCls("flex-1 truncate font-mono leading-9"),
                "select-all bg-surface-2 text-muted",
              )}
            >
              {wsPath ?? "available in the desktop app"}
            </span>
            {wsPath && (
              <>
                <button className={btnGhost("gap-1.5")} onClick={() => void changeWorkspaceBase()}>
                  Change…
                </button>
                <button className={btnGhost("gap-1.5")} onClick={() => void openWorkspaceBase()}>
                  <FolderOpen size={13} /> Reveal
                </button>
              </>
            )}
          </div>
        </Card>

        {/* ---- Cluster (HPC) ---- */}
        <ClusterCard />

        <ModalCard />

        {/* ---- Execution Backend (WSL / native) ---- */}
        <WslBackendCard />

        {/* ---- Privacy & data flow ---- */}
        <DataFlowCard model={defaultModel} workspace={wsPath} />

        {/* ---- Appearance ---- */}
        <Card title="Appearance">
          <div className="inline-flex rounded-input border border-border bg-surface-2 p-0.5">
            {(["light", "dark"] as const).map((t) => (
              <button
                key={t}
                onClick={() => setTheme(t)}
                className={cn(
                  "rounded-[5px] px-4 py-1.5 text-[13px] capitalize transition-colors",
                  theme === t ? "bg-surface text-text shadow-card" : "text-muted hover:text-text",
                )}
              >
                {t}
              </button>
            ))}
          </div>
        </Card>
      </div>
    </div>
  );
}

/* ---- Shared bits: one look for every control on this page ---- */

const inputCls = (extra = "") =>
  cn(
    "h-9 rounded-input border border-border bg-surface px-3 text-[13px] text-text outline-none",
    "placeholder:text-muted focus:border-accent/60",
    extra,
  );

const btnGhost = (extra = "") =>
  cn(
    "flex h-9 shrink-0 items-center gap-1 rounded-input border border-border bg-surface px-3.5",
    "text-[13px] text-text transition-colors hover:bg-surface-2 disabled:opacity-50",
    extra,
  );

const btnAccent = (extra = "") =>
  cn(
    "flex h-9 shrink-0 items-center gap-1.5 rounded-input bg-accent px-3.5 text-[13px] font-medium",
    "text-accent-fg transition-opacity hover:opacity-90 disabled:opacity-50",
    extra,
  );

function Card({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="mt-5 rounded-card border border-border bg-surface shadow-card">
      <header className="border-b border-border px-5 py-3">
        <h2 className="font-serif text-[15px] text-text">{title}</h2>
        {hint && <p className="mt-0.5 text-xs text-muted">{hint}</p>}
      </header>
      <div className="px-5 py-4">{children}</div>
    </section>
  );
}

function Divider({ label }: { label: string }) {
  return (
    <div className="mb-3 mt-5 flex items-center gap-3">
      <span className="text-xs font-medium uppercase tracking-wider text-muted">{label}</span>
      <span className="h-px flex-1 bg-border" />
    </div>
  );
}
