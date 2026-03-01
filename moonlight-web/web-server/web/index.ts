import "./polyfill/index.js";
import { apiGetApps, apiGetHosts, apiPostHost, getApi } from "./api.js";
import { showErrorPopup } from "./component/error.js";
import { buildUrl } from "./config_.js";
import CONFIG from "./config.js";

const DEFAULT_SUNSHINE_ADDRESS = "127.0.0.1";
const DEFAULT_SUNSHINE_HTTP_PORT = 47989;
const APPS_READY_TIMEOUT_MS = 30000;
const APPS_RETRY_INTERVAL_MS = 1000;

function pickDesktopApp(apps: Array<{ app_id: number; title: string }>): { app_id: number; title: string } | null {
  if (apps.length === 0) return null;

  const desktopMatch = apps.find((app) => app.title.toLowerCase().includes("desktop"));
  if (desktopMatch) return desktopMatch;

  return apps[0];
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms);
  });
}

function getSingleHostConfig(): { address: string; httpPort: number } {
  const defaults = (CONFIG?.default_settings ?? {}) as Record<string, unknown>;

  const rawAddress = defaults["single_host_address"] ?? defaults["singleHostAddress"];
  const address =
    typeof rawAddress === "string" && rawAddress.trim().length > 0
      ? rawAddress.trim()
      : DEFAULT_SUNSHINE_ADDRESS;

  const rawPort = defaults["single_host_http_port"] ?? defaults["singleHostHttpPort"];
  const parsedPort =
    typeof rawPort === "number" && Number.isInteger(rawPort) ? rawPort : DEFAULT_SUNSHINE_HTTP_PORT;
  const httpPort =
    parsedPort >= 1 && parsedPort <= 65535 ? parsedPort : DEFAULT_SUNSHINE_HTTP_PORT;

  return { address, httpPort };
}

async function resolveHost(api: Awaited<ReturnType<typeof getApi>>): Promise<{ host_id: number }> {
  const hostsResponse = await apiGetHosts(api);
  const existingHost = hostsResponse.response.hosts[0];
  if (existingHost) {
    return existingHost;
  }

  const { address, httpPort } = getSingleHostConfig();
  const createdHost = await apiPostHost(api, {
    address,
    http_port: httpPort,
  });
  return createdHost;
}

async function waitForAppsReady(
  api: Awaited<ReturnType<typeof getApi>>,
  hostId: number,
): Promise<Array<{ app_id: number; title: string }>> {
  const deadline = Date.now() + APPS_READY_TIMEOUT_MS;
  let lastError: unknown = null;

  while (Date.now() < deadline) {
    try {
      const apps = await apiGetApps(api, { host_id: hostId });
      if (apps.length > 0) {
        return apps;
      }
      lastError = new Error("host returned no launchable apps");
    } catch (error) {
      lastError = error;
    }

    await sleep(APPS_RETRY_INTERVAL_MS);
  }

  const lastReason = lastError instanceof Error ? lastError.message : `${lastError}`;
  throw new Error(`Sunshine host is not ready: ${lastReason}`);
}

async function startApp() {
  const api = await getApi();
  const host = await resolveHost(api);

  if (!host) {
    showErrorPopup("No host is available for streaming", true);
    return;
  }

  const apps = await waitForAppsReady(api, host.host_id);
  const selectedApp = pickDesktopApp(apps);
  if (!selectedApp) {
    showErrorPopup("No launchable app is available on the selected host", true);
    return;
  }

  const query = new URLSearchParams({
    hostId: host.host_id.toString(),
    appId: selectedApp.app_id.toString(),
    minimal: "1",
  });

  window.location.replace(buildUrl(`/stream.html?${query.toString()}`));
}

startApp().catch((error) => {
  const message = error instanceof Error ? error.message : `${error}`;
  showErrorPopup(`Failed to start stream: ${message}`, true);
});
