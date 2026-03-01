import "./polyfill/index.js";
import { apiGetApps, apiGetHosts, getApi } from "./api.js";
import { showErrorPopup } from "./component/error.js";
import { buildUrl } from "./config_.js";

function pickDesktopApp(apps: Array<{ app_id: number; title: string }>): { app_id: number; title: string } | null {
  if (apps.length === 0) return null;

  const desktopMatch = apps.find((app) => app.title.toLowerCase().includes("desktop"));
  if (desktopMatch) return desktopMatch;

  return apps[0];
}

async function startApp() {
  const api = await getApi();
  const hostsResponse = await apiGetHosts(api);
  const host = hostsResponse.response.hosts[0];

  if (!host) {
    showErrorPopup("No host is available for streaming", true);
    return;
  }

  const apps = await apiGetApps(api, { host_id: host.host_id });
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
