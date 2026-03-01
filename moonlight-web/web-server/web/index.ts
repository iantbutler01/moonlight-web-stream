import "./polyfill/index.js";
import { apiGetLocalBootstrap, extractLocalErrorResponse, getApi } from "./api.js";
import { buildUrl } from "./config_.js";

const RETRY_DELAY_INITIAL_MS = 100;
const RETRY_DELAY_MAX_MS = 2000;

type StatusWriter = (message: string) => void;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

function createStatusOverlay(): StatusWriter {
  const root = document.getElementById("root");
  if (root == null) {
    return () => null;
  }

  const container = document.createElement("div");
  container.style.height = "100%";
  container.style.display = "flex";
  container.style.alignItems = "center";
  container.style.justifyContent = "center";
  container.style.background = "radial-gradient(circle at 20% 10%, #0f172a, #020617)";

  const card = document.createElement("div");
  card.style.maxWidth = "720px";
  card.style.padding = "20px 24px";
  card.style.borderRadius = "12px";
  card.style.border = "1px solid rgba(148, 163, 184, 0.35)";
  card.style.background = "rgba(2, 6, 23, 0.72)";
  card.style.boxShadow = "0 20px 40px rgba(2, 6, 23, 0.45)";
  card.style.color = "#e2e8f0";
  card.style.fontFamily = "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace";
  card.style.fontSize = "14px";
  card.style.lineHeight = "1.4";
  card.style.whiteSpace = "pre-wrap";

  card.textContent = "Preparing local Sunshine stream...";
  container.appendChild(card);
  root.appendChild(container);

  return (message: string) => {
    card.textContent = message;
  };
}

async function waitForLocalBootstrap(setStatus: StatusWriter): Promise<{ host_id: number; app_id: number }> {
  const api = await getApi();
  let delay = RETRY_DELAY_INITIAL_MS;

  while (true) {
    try {
      const bootstrap = await apiGetLocalBootstrap(api);
      setStatus(bootstrap.status_text);
      return {
        host_id: bootstrap.host_id,
        app_id: bootstrap.app_id,
      };
    } catch (error) {
      const localError = await extractLocalErrorResponse(error);
      if (localError != null) {
        setStatus(`[${localError.code}] ${localError.status_text}\nretrying in ${delay}ms`);
      } else if (error instanceof Error) {
        setStatus(`${error.message}\nretrying in ${delay}ms`);
      } else {
        setStatus(`stream bootstrap failed\nretrying in ${delay}ms`);
      }
      await sleep(delay);
      delay = Math.min(delay * 2, RETRY_DELAY_MAX_MS);
    }
  }
}

async function startApp() {
  const setStatus = createStatusOverlay();
  setStatus("Preparing local Sunshine stream...");
  const bootstrap = await waitForLocalBootstrap(setStatus);

  const query = new URLSearchParams({
    hostId: bootstrap.host_id.toString(),
    appId: bootstrap.app_id.toString(),
    minimal: "1",
  });
  window.location.replace(buildUrl(`/stream.html?${query.toString()}`));
}

startApp().catch((error) => {
  const message = error instanceof Error ? error.message : `${error}`;
  const setStatus = createStatusOverlay();
  setStatus(`fatal startup error\n${message}`);
});
