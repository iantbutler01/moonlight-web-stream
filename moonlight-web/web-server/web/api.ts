import {
  GetLocalBootstrapResponse,
  GetLocalStatusResponse,
  LocalErrorResponse,
} from "./api_bindings.js";
import { buildUrl } from "./config_.js";

const API_TIMEOUT = 12000;

export async function getApi(): Promise<Api> {
  const host_url = buildUrl("/api");
  return { host_url };
}

const GET = "GET";
const POST = "POST";

export type Api = {
  host_url: string;
};

export type ApiFetchInit = {
  json?: unknown;
  query?: Record<string, unknown>;
  noTimeout?: boolean;
};

function buildRequest(api: Api, endpoint: string, method: string, init?: ApiFetchInit): [string, RequestInit] {
  const queryObj = init?.query || {};
  const queryParts: string[] = [];
  for (const key in queryObj) {
    if (queryObj[key] != null) {
      queryParts.push(`${encodeURIComponent(key)}=${encodeURIComponent(String(queryObj[key]))}`);
    }
  }
  const queryString = queryParts.length > 0 ? `?${queryParts.join("&")}` : "";

  const url = `${api.host_url}${endpoint}${queryString}`;

  const headers: Record<string, string> = {};
  if (init?.json) {
    headers["Content-Type"] = "application/json";
  }

  const request: RequestInit = {
    method,
    headers,
    body: init?.json ? JSON.stringify(init.json) : undefined,
    credentials: "include",
  };

  return [url, request];
}

export class FetchError extends Error {
  private response?: Response;

  constructor(type: "timeout", endpoint: string, method: string);
  constructor(type: "failed", endpoint: string, method: string, response: Response, reason?: string);
  constructor(type: "unknown", endpoint: string, method: string, error: Error);
  constructor(
    type: "timeout" | "failed" | "unknown",
    endpoint: string,
    method: string,
    responseOrError?: Response | Error,
    reason?: string,
  ) {
    if (type === "timeout") {
      super(`failed to fetch ${method} at ${endpoint} because of timeout`);
    } else if (type === "failed") {
      const response = responseOrError as Response;
      super(`failed to fetch ${method} at ${endpoint} with code ${response?.status} ${reason ? `because of ${reason}` : ""}`);
      this.response = response;
    } else {
      const error = responseOrError as Error;
      super(`failed to fetch ${method} at ${endpoint} because of ${error}`);
    }
  }

  getResponse(): Response | null {
    return this.response ?? null;
  }
}

export async function fetchApi(api: Api, endpoint: string, method: string, init: { response: "ignore" } & ApiFetchInit, timeout?: number): Promise<Response>;
export async function fetchApi(api: Api, endpoint: string, method?: string, init?: { response?: "json" } & ApiFetchInit, timeout?: number): Promise<unknown>;

export async function fetchApi(
  api: Api,
  endpoint: string,
  method: string = GET,
  init?: { response?: "json" | "ignore" } & ApiFetchInit,
  timeout: number = API_TIMEOUT,
): Promise<unknown | Response> {
  const [url, request] = buildRequest(api, endpoint, method, init);

  if (!init?.noTimeout) {
    request.signal = AbortSignal.timeout(timeout);
  }

  let response: Response;
  try {
    response = await fetch(url, request);
  } catch (error: unknown) {
    if (error instanceof Error && error.name === "TimeoutError") {
      throw new FetchError("timeout", endpoint, method);
    }
    throw new FetchError("unknown", endpoint, method, error as Error);
  }

  if (!response.ok) {
    throw new FetchError("failed", endpoint, method, response);
  }

  if (init?.response === "ignore") {
    return response;
  }

  return response.json();
}

export async function apiGetLocalStatus(api: Api): Promise<GetLocalStatusResponse> {
  const response = await fetchApi(api, "/local/status", GET);
  return response as GetLocalStatusResponse;
}

export async function apiPostLocalEnsureReady(api: Api): Promise<GetLocalBootstrapResponse> {
  const response = await fetchApi(api, "/local/ensure_ready", POST, {
    response: "json",
  });
  return response as GetLocalBootstrapResponse;
}

export async function apiGetLocalBootstrap(api: Api): Promise<GetLocalBootstrapResponse> {
  const response = await fetchApi(api, "/local/bootstrap", GET);
  return response as GetLocalBootstrapResponse;
}

export async function extractLocalErrorResponse(error: unknown): Promise<LocalErrorResponse | null> {
  if (!(error instanceof FetchError)) {
    return null;
  }
  const response = error.getResponse();
  if (response == null) {
    return null;
  }
  try {
    return (await response.json()) as LocalErrorResponse;
  } catch {
    return null;
  }
}
