// Setup / onboarding page: point traffic at the proxy, install the CA cert,
// then verify. Deliberately spacious — this is the one page meant to be
// read start-to-finish once, not scanned like a data table.

import type { Component } from "solid-js";
import { Show, createEffect, createResource, createSignal } from "solid-js";
import "../styles/setup.css";
import { getSetupInfo, getState } from "../lib/api";
import { triggerDownload } from "../lib/download";
import { pushToast } from "../stores/ui";
import Tabs from "../components/Tabs";
import Button from "../components/Button";

type PlatformTab = "macos" | "windows" | "linux" | "ios" | "android" | "curl";

const PROXY_TABS: { id: PlatformTab; label: string }[] = [
  { id: "macos", label: "macOS" },
  { id: "windows", label: "Windows" },
  { id: "linux", label: "Linux" },
  { id: "ios", label: "iOS" },
  { id: "android", label: "Android" },
  { id: "curl", label: "cURL / env vars" },
];

const TRUST_TABS: { id: PlatformTab; label: string }[] = [
  { id: "macos", label: "macOS" },
  { id: "windows", label: "Windows" },
  { id: "linux", label: "Linux" },
  { id: "ios", label: "iOS" },
  { id: "android", label: "Android" },
];

// `<pre>` text spanning multiple JSX lines gets its newlines collapsed to a
// single space by the JSX whitespace algorithm (JSX isn't `<pre>`-aware) —
// build these as one JS string with real `\n`s instead of literal JSX text.
function curlSnippet(host: string, port: number): string {
  return `export https_proxy=http://${host}:${port}\nexport http_proxy=http://${host}:${port}`;
}

const LINUX_TRUST_SNIPPET = "sudo cp rdproxy-ca.crt /usr/local/share/ca-certificates/\nsudo update-ca-certificates";

async function copy(text: string, label: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    pushToast({ level: "success", message: `Copied ${label}` });
  } catch {
    pushToast({ level: "error", message: `Failed to copy ${label}` });
  }
}

const Setup: Component = () => {
  const [setupInfo, { refetch: refetchSetupInfo }] = createResource(getSetupInfo);
  const [state, { refetch: refetchState }] = createResource(getState);

  const [proxyTab, setProxyTab] = createSignal<PlatformTab>("macos");
  const [trustTab, setTrustTab] = createSignal<PlatformTab>("macos");
  const [baselineFlowCount, setBaselineFlowCount] = createSignal<number | null>(null);
  const [checking, setChecking] = createSignal(false);
  const [checkResult, setCheckResult] = createSignal<string | null>(null);

  createEffect(() => {
    const s = state();
    if (s && baselineFlowCount() === null) setBaselineFlowCount(s.flowCount);
  });

  const hostPort = () => {
    const info = setupInfo();
    return info ? `${info.proxyHost}:${info.proxyPort}` : "";
  };

  const onTestConnection = async () => {
    setChecking(true);
    try {
      const fresh = await getState();
      const baseline = baselineFlowCount() ?? fresh.flowCount;
      const delta = fresh.flowCount - baseline;
      setCheckResult(
        delta > 0
          ? `${delta} new flow${delta === 1 ? "" : "s"} captured since this page opened — it's working.`
          : "No new traffic seen yet. Browse using the configured proxy, then check again, or watch the Traffic tab directly.",
      );
      await refetchState();
    } catch {
      setCheckResult("Could not reach rdproxy's backend — is it still running?");
    } finally {
      setChecking(false);
    }
  };

  return (
    <div class="setup-page">
      <div class="setup-page__intro">
        <h1>Set up rdproxy</h1>
        <p>Point a device at this proxy, install its certificate, then verify traffic shows up.</p>
      </div>

      <Show
        when={setupInfo()}
        fallback={
          <div class="setup-page__loading">
            <Show when={setupInfo.error} fallback="Loading setup info…">
              <p>Could not reach rdproxy's backend.</p>
              <Button variant="default" size="sm" onClick={() => void refetchSetupInfo()}>
                Retry
              </Button>
            </Show>
          </div>
        }
      >
        {(info) => (
          <>
            <section class="setup-step">
              <div class="setup-step__number">1</div>
              <div class="setup-step__content">
                <h2>Point traffic at the proxy</h2>
                <div class="setup-page__hostport-row">
                  <code class="setup-page__hostport">{hostPort()}</code>
                  <Button variant="default" size="sm" icon="copy" onClick={() => void copy(hostPort(), "proxy address")}>
                    Copy
                  </Button>
                </div>
                <Show when={info().lanAddresses.length > 0}>
                  <p class="setup-page__note">
                    Reachable on your LAN at: {info().lanAddresses.map((a) => `${a}:${info().proxyPort}`).join(", ")}
                  </p>
                </Show>

                <Tabs tabs={PROXY_TABS} active={proxyTab()} onChange={(id) => setProxyTab(id as PlatformTab)}>
                  <Show when={proxyTab() === "macos"}>
                    <ol class="setup-instructions">
                      <li>System Settings → Wi-Fi → Details (for your active network) → Proxies.</li>
                      <li>Enable "Web Proxy (HTTP)" and "Secure Web Proxy (HTTPS)".</li>
                      <li>
                        Set server to <code>{info().proxyHost}</code> and port to <code>{info().proxyPort}</code>.
                      </li>
                    </ol>
                  </Show>
                  <Show when={proxyTab() === "windows"}>
                    <ol class="setup-instructions">
                      <li>Settings → Network &amp; Internet → Proxy.</li>
                      <li>Under "Manual proxy setup", turn on "Use a proxy server".</li>
                      <li>
                        Address <code>{info().proxyHost}</code>, port <code>{info().proxyPort}</code>, then Save.
                      </li>
                    </ol>
                  </Show>
                  <Show when={proxyTab() === "linux"}>
                    <ol class="setup-instructions">
                      <li>Settings → Network → Network Proxy → Manual.</li>
                      <li>
                        Set HTTP and HTTPS proxy to <code>{info().proxyHost}</code> port <code>{info().proxyPort}</code>.
                      </li>
                      <li>Or export the environment variables in the cURL tab for a single shell session.</li>
                    </ol>
                  </Show>
                  <Show when={proxyTab() === "ios"}>
                    <ol class="setup-instructions">
                      <li>Settings → Wi-Fi → tap the (i) next to your network.</li>
                      <li>Configure Proxy → Manual.</li>
                      <li>
                        Server <code>{info().proxyHost}</code>, Port <code>{info().proxyPort}</code>.
                      </li>
                    </ol>
                  </Show>
                  <Show when={proxyTab() === "android"}>
                    <ol class="setup-instructions">
                      <li>Long-press your Wi-Fi network → Modify network.</li>
                      <li>Advanced options → Proxy → Manual.</li>
                      <li>
                        Proxy hostname <code>{info().proxyHost}</code>, Proxy port <code>{info().proxyPort}</code>.
                      </li>
                    </ol>
                  </Show>
                  <Show when={proxyTab() === "curl"}>
                    <pre class="setup-page__code mono">{curlSnippet(info().proxyHost, info().proxyPort)}</pre>
                  </Show>
                </Tabs>
              </div>
            </section>

            <section class="setup-step">
              <div class="setup-step__number">2</div>
              <div class="setup-step__content">
                <h2>Install the CA certificate</h2>
                <Show when={info().qrSvg}>
                  <div class="setup-page__qr" innerHTML={info().qrSvg} />
                </Show>
                <div class="setup-page__hostport-row">
                  <code class="setup-page__hostport">{info().certUrl}</code>
                  <Button variant="default" size="sm" icon="copy" onClick={() => void copy(info().certUrl, "certificate URL")}>
                    Copy
                  </Button>
                </div>
                <div class="setup-page__downloads">
                  <Button variant="default" size="sm" icon="download" onClick={() => triggerDownload("/cert/rdproxy-ca.pem", "rdproxy-ca.pem")}>
                    Download .pem
                  </Button>
                  <Button variant="default" size="sm" icon="download" onClick={() => triggerDownload("/cert/rdproxy-ca.crt", "rdproxy-ca.crt")}>
                    Download .crt
                  </Button>
                </div>
                <div class="setup-page__hostport-row">
                  <span class="setup-page__note">SHA-256 fingerprint:</span>
                  <code class="setup-page__fingerprint">{info().caFingerprint}</code>
                  <Button variant="ghost" size="sm" icon="copy" aria-label="Copy fingerprint" onClick={() => void copy(info().caFingerprint, "fingerprint")} />
                </div>

                <Tabs tabs={TRUST_TABS} active={trustTab()} onChange={(id) => setTrustTab(id as PlatformTab)}>
                  <Show when={trustTab() === "macos"}>
                    <ol class="setup-instructions">
                      <li>Double-click the downloaded certificate to open Keychain Access.</li>
                      <li>Find "rdproxy" under System (or login) keychain.</li>
                      <li>Double-click it → Trust → set "When using this certificate" to Always Trust.</li>
                    </ol>
                  </Show>
                  <Show when={trustTab() === "windows"}>
                    <pre class="setup-page__code mono">certutil -addstore -f root rdproxy-ca.crt</pre>
                    <p class="setup-page__note">Run from an elevated (Administrator) command prompt.</p>
                  </Show>
                  <Show when={trustTab() === "linux"}>
                    <pre class="setup-page__code mono">{LINUX_TRUST_SNIPPET}</pre>
                  </Show>
                  <Show when={trustTab() === "ios"}>
                    <ol class="setup-instructions">
                      <li>
                        Open <code>{info().certUrl}</code> in Safari on the device → Allow.
                      </li>
                      <li>Settings → Profile Downloaded → Install (top right) → Install.</li>
                      <li class="setup-instructions__emphasis">
                        Then go to Settings → General → About → Certificate Trust Settings → enable full trust for the rdproxy
                        root certificate. Skipping this step is the #1 reason HTTPS interception silently fails on iOS.
                      </li>
                    </ol>
                  </Show>
                  <Show when={trustTab() === "android"}>
                    <ol class="setup-instructions">
                      <li>Download the certificate, then go to Settings → Security → Encryption &amp; credentials.</li>
                      <li>Install a certificate → CA certificate → select the downloaded file.</li>
                      <li class="setup-instructions__emphasis">
                        Android 7+ apps ignore user-installed CAs by default unless the app explicitly opts in. For full
                        interception you'll need an emulator, a rooted device, or a system-store cert install.
                      </li>
                    </ol>
                  </Show>
                </Tabs>
              </div>
            </section>

            <section class="setup-step">
              <div class="setup-step__number">3</div>
              <div class="setup-step__content">
                <h2>Verify</h2>
                <p>Browse a page or make a request using the configured proxy, then check for new traffic.</p>
                <div class="setup-page__hostport-row">
                  <Button variant="primary" size="sm" onClick={() => void onTestConnection()} disabled={checking()}>
                    {checking() ? "Checking…" : "Check for new traffic"}
                  </Button>
                  <span class="setup-page__note">Total flows captured: {state()?.flowCount ?? "—"}</span>
                </div>
                <Show when={checkResult()}>{(msg) => <p class="setup-page__result">{msg()}</p>}</Show>
              </div>
            </section>

            <div class="setup-page__caveat">
              <strong>Note:</strong> certificate-pinned apps (banking apps, some messengers) will still fail to connect
              through the proxy even with the CA trusted — that's expected and by design on their part.
            </div>
          </>
        )}
      </Show>
    </div>
  );
};

export default Setup;
