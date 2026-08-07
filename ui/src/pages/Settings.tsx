// Settings page: grouped sections, each field persisted via a debounced
// PUT /api/settings partial patch (never the whole object). Local editable
// state is seeded once from the first successful load and is the source of
// truth for display afterward — this avoids the classic "server response
// clobbers what I'm mid-typing" controlled-input fight.

import type { Component } from "solid-js";
import { For, Show, createEffect, createResource, createSignal, onCleanup } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import "../styles/settings.css";
import type { PassthroughPreset, Rule, Settings as SettingsType } from "../lib/types";
import * as api from "../lib/api";
import { getHar, getPassthroughPresets, importHar as importHarApi } from "../lib/api";
import { apiState, systemProxyBusy, toggleSystemProxy } from "../stores/systemProxy";
import { triggerDownload } from "../lib/download";
import { formatBytes, formatDuration } from "../lib/format";
import { pushToast } from "../stores/ui";
import {
  setSettings,
  settings,
  settingsLoadError,
  settingsRestartRequired,
  dismissRestartRequired,
  setTheme,
  theme,
  refetchSettings,
  type ThemeChoice,
} from "../stores/settings";
import { allFlowIds, clearFlows as clearFlowsStore } from "../stores/flows";
import { exportRules as exportRulesAction, importRules as importRulesAction } from "../stores/rules";
import TextInput from "../components/TextInput";
import Toggle from "../components/Toggle";
import Select from "../components/Select";
import Button from "../components/Button";
import Modal from "../components/Modal";
import RepeatableInputList from "../components/RepeatableInputList";

const EMPTY_SETTINGS: SettingsType = {
  proxyPort: 0,
  uiPort: 0,
  bindAddr: "",
  maxFlows: 0,
  maxBodyBytes: 0,
  interceptHttps: false,
  passthroughHosts: [],
  passthroughPresets: [],
  captureIncludeHosts: [],
  captureExcludeHosts: [],
  manualProxy: false,
  systemProxyBypass: [],
  captureWebsockets: false,
  theme: "dark",
  upstreamProxy: null,
  paused: false,
};

const FLUSH_MS = 400;

const Settings: Component = () => {
  const [local, setLocal] = createStore<SettingsType>({ ...EMPTY_SETTINGS });
  const [initialized, setInitialized] = createSignal(false);
  const [passthroughPresets] = createResource(getPassthroughPresets);
  const [clearConfirmOpen, setClearConfirmOpen] = createSignal(false);
  const [importRulesPending, setImportRulesPending] = createSignal<Rule[] | null>(null);
  const [importRulesReplace, setImportRulesReplace] = createSignal(false);

  let harFileInput: HTMLInputElement | undefined;
  let rulesFileInput: HTMLInputElement | undefined;

  createEffect(() => {
    const s = settings();
    if (s && !initialized()) {
      setLocal(reconcile(s));
      setInitialized(true);
    }
  });

  // ---- debounced partial patch queue ----
  let pending: Partial<SettingsType> = {};
  let flushTimer: ReturnType<typeof setTimeout> | undefined;

  const flush = () => {
    const patch = pending;
    pending = {};
    flushTimer = undefined;
    if (Object.keys(patch).length === 0) return;
    setSettings(patch).catch(() => pushToast({ level: "error", message: "Failed to save settings" }));
  };

  const queuePatch = (patch: Partial<SettingsType>) => {
    pending = { ...pending, ...patch };
    if (flushTimer !== undefined) clearTimeout(flushTimer);
    flushTimer = setTimeout(flush, FLUSH_MS);
  };

  onCleanup(() => {
    if (flushTimer !== undefined) {
      clearTimeout(flushTimer);
      flush();
    }
  });

  function setField<K extends keyof SettingsType>(key: K, value: SettingsType[K]): void {
    setLocal(key, value);
    queuePatch({ [key]: value } as Partial<SettingsType>);
  }

  const onTogglePassthroughPreset = (name: string, enabled: boolean) => {
    const next = enabled ? [...local.passthroughPresets, name] : local.passthroughPresets.filter((n) => n !== name);
    setField("passthroughPresets", next);
  };

  // ---- system proxy ----
  // Shared store (../stores/systemProxy) so this toggle and the Toolbar's
  // always-visible control read/write the same state and can never disagree.

  // ---- data: HAR export/import ----
  const onExportHarAll = async () => {
    const ids = allFlowIds();
    if (ids.length === 0) {
      pushToast({ level: "warning", message: "No flows captured yet" });
      return;
    }
    try {
      const { blob, filename } = await getHar(ids);
      triggerDownload(blob, filename ?? "hamsy-session.har");
    } catch {
      pushToast({ level: "error", message: "Failed to export HAR" });
    }
  };

  const onImportHarFile = async (e: Event) => {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;
    try {
      const text = await file.text();
      const parsed: unknown = JSON.parse(text);
      const result = await importHarApi(parsed);
      pushToast({ level: "success", message: `Imported ${result.imported} flow(s)` });
    } catch {
      pushToast({ level: "error", message: "Failed to import HAR" });
    }
  };

  // ---- data: rules export/import ----
  const onExportRules = async () => {
    try {
      const { rules: exported } = await exportRulesAction();
      triggerDownload(new Blob([JSON.stringify({ rules: exported }, null, 2)], { type: "application/json" }), "hamsy-rules.json");
    } catch {
      pushToast({ level: "error", message: "Failed to export rules" });
    }
  };

  const onRulesFileChange = async (e: Event) => {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;
    try {
      const text = await file.text();
      const parsed: unknown = JSON.parse(text);
      const list = Array.isArray(parsed) ? parsed : (parsed as { rules?: unknown }).rules;
      if (!Array.isArray(list)) throw new Error("Expected an array of rules");
      setImportRulesPending(list as Rule[]);
      setImportRulesReplace(false);
    } catch {
      pushToast({ level: "error", message: "Failed to parse rules file" });
    }
  };

  const confirmImportRules = async () => {
    const pendingRules = importRulesPending();
    if (!pendingRules) return;
    try {
      const result = await importRulesAction(pendingRules, importRulesReplace());
      pushToast({ level: "success", message: `Imported ${result.imported} rule(s)` });
      setImportRulesPending(null);
    } catch {
      pushToast({ level: "error", message: "Failed to import rules" });
    }
  };

  // ---- data: clear flows ----
  const onClearFlows = async () => {
    setClearConfirmOpen(false);
    clearFlowsStore();
    try {
      await api.clearFlows();
    } catch {
      pushToast({ level: "error", message: "Failed to clear flows on the server" });
    }
  };

  return (
    <div class="settings-page">
      <Show when={settingsRestartRequired()}>
        <div class="settings-page__banner" role="status">
          <span>Restart hamsy-proxy for the new port/bind-address to take effect.</span>
          <Button variant="ghost" size="sm" icon="close" aria-label="Dismiss" onClick={dismissRestartRequired} />
        </div>
      </Show>

      <Show
        when={initialized()}
        fallback={
          <div class="settings-page__loading">
            <Show when={settingsLoadError()} fallback="Loading settings…">
              <p>Could not reach hamsy-proxy's backend.</p>
              <Button variant="default" size="sm" onClick={() => void refetchSettings()}>
                Retry
              </Button>
            </Show>
          </div>
        }
      >
        <div class="settings-page__body">
          <section class="settings-section">
            <h2 class="settings-section__title">Proxy</h2>
            <div class="settings-field">
              <label class="settings-field__label" for="settings-proxy-port">
                Proxy port
              </label>
              <TextInput
                id="settings-proxy-port"
                type="number"
                mono
                value={String(local.proxyPort)}
                onInput={(v) => setField("proxyPort", Number(v) || 0)}
              />
              <p class="settings-field__desc">Port the MITM proxy listens on.</p>
            </div>
            <div class="settings-field">
              <label class="settings-field__label" for="settings-ui-port">
                UI port
              </label>
              <TextInput id="settings-ui-port" type="number" mono value={String(local.uiPort)} onInput={(v) => setField("uiPort", Number(v) || 0)} />
              <p class="settings-field__desc">Port this dashboard is served on.</p>
            </div>
            <div class="settings-field">
              <label class="settings-field__label" for="settings-bind-addr">
                Bind address
              </label>
              <TextInput id="settings-bind-addr" mono value={local.bindAddr} onInput={(v) => setField("bindAddr", v)} />
              <p class="settings-field__desc">
                <Show when={local.bindAddr === "0.0.0.0"}>
                  <span class="settings-field__warning">0.0.0.0 exposes the proxy to your LAN — required for phones/tablets to connect.</span>
                </Show>
                <Show when={local.bindAddr !== "0.0.0.0"}>Use 0.0.0.0 to allow other devices on your network (phones, tablets) to connect.</Show>
              </p>
            </div>
            <div class="settings-field">
              <label class="settings-field__label" for="settings-upstream-proxy">
                Upstream proxy
              </label>
              <TextInput
                id="settings-upstream-proxy"
                mono
                placeholder="none"
                value={local.upstreamProxy ?? ""}
                onInput={(v) => setField("upstreamProxy", v || null)}
              />
              <p class="settings-field__desc">Forward all traffic through another proxy (e.g. http://127.0.0.1:8080).</p>
            </div>
          </section>

          <section class="settings-section">
            <h2 class="settings-section__title">Capture</h2>
            <div class="settings-field settings-field--row">
              <Toggle checked={local.paused} onChange={(v) => setField("paused", v)} label="Paused" />
              <p class="settings-field__desc">Stop recording new traffic without closing the proxy.</p>
            </div>
            <div class="settings-field">
              <label class="settings-field__label" for="settings-max-flows">
                Max flows kept in memory
              </label>
              <TextInput id="settings-max-flows" type="number" mono value={String(local.maxFlows)} onInput={(v) => setField("maxFlows", Number(v) || 0)} />
              <p class="settings-field__desc">Older flows are evicted once this limit is reached. Higher values use more memory.</p>
            </div>
            <div class="settings-field">
              <label class="settings-field__label" for="settings-max-body">
                Max body size (MB)
              </label>
              <TextInput
                id="settings-max-body"
                type="number"
                mono
                value={String(Math.round((local.maxBodyBytes / (1024 * 1024)) * 100) / 100)}
                onInput={(v) => setField("maxBodyBytes", Math.round((Number(v) || 0) * 1024 * 1024))}
              />
              <p class="settings-field__desc">Bodies larger than this are truncated ({formatBytes(local.maxBodyBytes)}).</p>
            </div>
            <div class="settings-field settings-field--row">
              <Toggle checked={local.captureWebsockets} onChange={(v) => setField("captureWebsockets", v)} label="Capture WebSocket frames" />
            </div>
            <div class="settings-field">
              <span class="settings-field__label">Include hosts (glob)</span>
              <RepeatableInputList
                values={local.captureIncludeHosts}
                onChange={(v) => setField("captureIncludeHosts", v)}
                placeholder="*.example.com"
                mono
                addLabel="Add host"
                aria-label="Include host glob"
              />
              <p class="settings-field__desc">Empty = capture all hosts.</p>
            </div>
            <div class="settings-field">
              <span class="settings-field__label">Exclude hosts (glob)</span>
              <RepeatableInputList
                values={local.captureExcludeHosts}
                onChange={(v) => setField("captureExcludeHosts", v)}
                placeholder="*.ads.example.com"
                mono
                addLabel="Add host"
                aria-label="Exclude host glob"
              />
            </div>
          </section>

          <section class="settings-section">
            <h2 class="settings-section__title">HTTPS</h2>
            <div class="settings-field settings-field--row">
              <Toggle checked={local.interceptHttps} onChange={(v) => setField("interceptHttps", v)} label="Intercept HTTPS" />
            </div>
            <div class="settings-field">
              <span class="settings-field__label">Passthrough presets</span>
              <p class="settings-field__desc">
                Built-in groups of hosts that are never MITM'd — cloud CLIs, container registries, and apps that pin their
                certificate or ship their own CA bundle all break outright under interception. Their hosts are unioned with the
                manual list below.
              </p>
              <div class="settings-preset-list">
                <For each={passthroughPresets()}>
                  {(preset: PassthroughPreset) => (
                    <div class="settings-preset-list__row">
                      <Toggle
                        checked={local.passthroughPresets.includes(preset.name)}
                        onChange={(v) => onTogglePassthroughPreset(preset.name, v)}
                        label={preset.label}
                      />
                      <p class="settings-field__desc">
                        {preset.description} ({preset.hosts.length} hosts)
                      </p>
                    </div>
                  )}
                </For>
              </div>
            </div>
            <div class="settings-field">
              <span class="settings-field__label">Passthrough hosts (glob)</span>
              <RepeatableInputList
                values={local.passthroughHosts}
                onChange={(v) => setField("passthroughHosts", v)}
                placeholder="*.bank.com"
                mono
                addLabel="Add host"
                aria-label="Passthrough host glob"
              />
              <p class="settings-field__desc">
                These hosts are tunneled without MITM (e.g. certificate-pinned apps), in addition to whatever the presets above
                cover.
              </p>
            </div>
            <div class="settings-field">
              <span class="settings-field__label">CA fingerprint</span>
              <div class="settings-field__row">
                <code class="settings-field__mono-value">{apiState()?.caFingerprint ?? "—"}</code>
                <Button
                  variant="ghost"
                  size="sm"
                  icon="copy"
                  aria-label="Copy CA fingerprint"
                  onClick={() => {
                    const fp = apiState()?.caFingerprint;
                    if (!fp) return;
                    navigator.clipboard.writeText(fp).then(
                      () => pushToast({ level: "success", message: "Copied fingerprint" }),
                      () => pushToast({ level: "error", message: "Failed to copy" }),
                    );
                  }}
                />
                <a href="/setup" class="settings-field__link">
                  Install certificate →
                </a>
              </div>
            </div>
          </section>

          <section class="settings-section">
            <h2 class="settings-section__title">System proxy</h2>
            <div class="settings-field settings-field--row" title={apiState()?.systemProxy.supported === false ? "Not supported on this platform" : undefined}>
              <Toggle
                checked={apiState()?.systemProxy.enabled ?? false}
                disabled={systemProxyBusy() || apiState()?.systemProxy.supported === false}
                onChange={(v) => void toggleSystemProxy(v)}
                label="Set as system proxy"
              />
              <p class="settings-field__desc">
                Same control as the "System proxy" button in the toolbar. Platform: {apiState()?.systemProxy.platform ?? "unknown"}
                <Show when={apiState()?.systemProxy.supported === false}> — not supported on this platform.</Show>
                <Show when={apiState()?.systemProxy.supported !== false}>
                  {" "}
                  Only affects clients relying on the OS proxy setting — clients pointed at 127.0.0.1:{apiState()?.proxyPort ?? "?"}{" "}
                  directly keep being captured either way.
                </Show>
              </p>
            </div>
            <div class="settings-field settings-field--row">
              <Toggle checked={local.manualProxy} onChange={(v) => setField("manualProxy", v)} label="Manual proxy setup" />
              <p class="settings-field__desc">Don't change the OS system proxy on startup; configure clients yourself.</p>
            </div>
            <div class="settings-field">
              <span class="settings-field__label">System proxy bypass hosts</span>
              <RepeatableInputList
                values={local.systemProxyBypass}
                onChange={(v) => setField("systemProxyBypass", v)}
                placeholder="localhost"
                mono
                addLabel="Add host"
                aria-label="System proxy bypass host"
              />
              <p class="settings-field__desc">
                Hosts listed here bypass the proxy at the OS level, so their traffic is never captured. Loopback is listed by
                default so hamsy doesn't route its own UI traffic through itself. Clearing this list won't by itself make Chrome
                or Firefox proxy their localhost requests — browsers bypass loopback internally regardless of this setting.
              </p>
            </div>
          </section>

          <section class="settings-section">
            <h2 class="settings-section__title">Appearance</h2>
            <div class="settings-field">
              <span class="settings-field__label">Theme</span>
              <Select
                value={theme()}
                onChange={(v) => setTheme(v as ThemeChoice)}
                options={[
                  { value: "dark", label: "Dark" },
                  { value: "light", label: "Light" },
                  { value: "system", label: "System" },
                ]}
              />
            </div>
          </section>

          <section class="settings-section">
            <h2 class="settings-section__title">Data</h2>
            <div class="settings-field settings-field--row">
              <Button variant="default" size="sm" icon="download" onClick={() => void onExportHarAll()}>
                Export HAR
              </Button>
              <Button variant="default" size="sm" icon="arrow-up" onClick={() => harFileInput?.click()}>
                Import HAR
              </Button>
              <input ref={harFileInput} type="file" accept=".har,application/json" class="settings-page__file-input" onChange={(e) => void onImportHarFile(e)} />
            </div>
            <div class="settings-field settings-field--row">
              <Button variant="default" size="sm" icon="download" onClick={() => void onExportRules()}>
                Export rules
              </Button>
              <Button variant="default" size="sm" icon="arrow-up" onClick={() => rulesFileInput?.click()}>
                Import rules
              </Button>
              <input ref={rulesFileInput} type="file" accept=".json,application/json" class="settings-page__file-input" onChange={(e) => void onRulesFileChange(e)} />
            </div>
            <div class="settings-field settings-field--row">
              <Button variant="danger" size="sm" icon="trash" onClick={() => setClearConfirmOpen(true)}>
                Clear all flows
              </Button>
            </div>
          </section>

          <section class="settings-section">
            <h2 class="settings-section__title">About</h2>
            <dl class="settings-about">
              <dt>Version</dt>
              <dd>{apiState()?.version ?? "—"}</dd>
              <dt>Uptime</dt>
              <dd>{apiState() ? formatDuration(apiState()!.uptimeSecs * 1000) : "—"}</dd>
              <dt>Flows captured</dt>
              <dd>{apiState()?.flowCount ?? "—"}</dd>
            </dl>
          </section>
        </div>
      </Show>

      <Modal open={clearConfirmOpen()} onClose={() => setClearConfirmOpen(false)} title="Clear all flows?">
        <div class="settings-confirm">
          <p>This removes every captured flow from this session. This cannot be undone.</p>
          <div class="settings-confirm__actions">
            <Button variant="ghost" size="sm" onClick={() => setClearConfirmOpen(false)}>
              Cancel
            </Button>
            <Button variant="danger" size="sm" onClick={() => void onClearFlows()}>
              Clear flows
            </Button>
          </div>
        </div>
      </Modal>

      <Modal open={importRulesPending() !== null} onClose={() => setImportRulesPending(null)} title="Import rules">
        <div class="settings-confirm">
          <p>{importRulesPending()?.length ?? 0} rule(s) found in the file.</p>
          <label class="settings-confirm__option">
            <input type="radio" name="settings-import-mode" checked={!importRulesReplace()} onChange={() => setImportRulesReplace(false)} />
            Merge with existing rules
          </label>
          <label class="settings-confirm__option">
            <input type="radio" name="settings-import-mode" checked={importRulesReplace()} onChange={() => setImportRulesReplace(true)} />
            Replace all existing rules
          </label>
          <div class="settings-confirm__actions">
            <Button variant="ghost" size="sm" onClick={() => setImportRulesPending(null)}>
              Cancel
            </Button>
            <Button variant="primary" size="sm" onClick={() => void confirmImportRules()}>
              Import
            </Button>
          </div>
        </div>
      </Modal>
    </div>
  );
};

export default Settings;
