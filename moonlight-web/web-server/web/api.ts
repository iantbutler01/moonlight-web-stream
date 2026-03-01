import {
  App,
  DeleteHostQuery,
  DetailedHost,
  GetAppImageQuery,
  GetAppsQuery,
  GetAppsResponse,
  GetHostQuery,
  GetHostResponse,
  GetHostsResponse,
  PostCancelRequest,
  PostCancelResponse,
  PostHostRequest,
  PostHostResponse,
  PostPairRequest,
  PostPairResponse1,
  PostPairResponse2,
  PostWakeUpRequest,
  UndetailedHost,
} from "./api_bindings.js";
import { buildUrl } from "./config_.js";

// IMPORTANT: this should be a bit bigger than the moonlight-common reqwest backend timeout if some hosts are offline!
const API_TIMEOUT = 12000;

export async function getApi(): Promise<Api> {
  const host_url = buildUrl("/api");
  return { host_url };
}

const GET = "GET";
const POST = "POST";
const DELETE = "DELETE";

export type Api = {
  host_url: string;
};

export type ApiFetchInit = {
  json?: unknown;
  query?: Record<string, unknown>;
  noTimeout?: boolean;
};

export function isDetailedHost(host: UndetailedHost | DetailedHost): host is DetailedHost {
  return (host as DetailedHost).https_port !== undefined;
}

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

class StreamedJsonResponse<Initial, Other> {
  response: Initial;

  private reader: ReadableStreamDefaultReader<Uint8Array>;
  private decoder = new TextDecoder();
  private bufferedText = "";

  constructor(body: ReadableStreamDefaultReader<Uint8Array>, response: Initial) {
    this.reader = body;
    this.response = response;
  }

  async next(): Promise<Other | null> {
    while (true) {
      const { done, value } = await this.reader.read();

      if (done) {
        return null;
      }

      this.bufferedText += this.decoder.decode(value);

      const split = this.bufferedText.split("\n", 2);
      if (split.length === 2) {
        this.bufferedText = split[1];
        return JSON.parse(split[0]) as Other;
      }
    }
  }
}

export async function fetchApi(api: Api, endpoint: string, method: string, init?: { response?: "json" } & ApiFetchInit, timeout?: number): Promise<unknown>;
export async function fetchApi(api: Api, endpoint: string, method: string, init: { response: "ignore" } & ApiFetchInit, timeout?: number): Promise<Response>;
export async function fetchApi<Initial, Other>(api: Api, endpoint: string, method: string, init: { response: "jsonStreaming" } & ApiFetchInit, timeout?: number): Promise<StreamedJsonResponse<Initial, Other>>;

export async function fetchApi<Initial = unknown, Other = unknown>(
  api: Api,
  endpoint: string,
  method: string = GET,
  init?: { response?: "json" | "ignore" | "jsonStreaming" } & ApiFetchInit,
  timeout: number = API_TIMEOUT,
) {
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

  if (init?.response == null || init.response === "json") {
    return response.json();
  }

  if (!response.body) {
    throw new FetchError("failed", endpoint, method, response);
  }

  const stream = new StreamedJsonResponse<Initial, Other>(response.body.getReader(), {} as Initial);
  const first = await stream.next();
  stream.response = first as Initial;
  return stream;
}

export async function apiGetHosts(api: Api): Promise<StreamedJsonResponse<GetHostsResponse, UndetailedHost>> {
  return fetchApi<GetHostsResponse, UndetailedHost>(api, "/hosts", GET, { response: "jsonStreaming" });
}

export async function apiGetHost(api: Api, query: GetHostQuery): Promise<DetailedHost> {
  const response = await fetchApi(api, "/host", GET, { query });
  return (response as GetHostResponse).host;
}

export async function apiPostHost(api: Api, data: PostHostRequest): Promise<DetailedHost> {
  const response = await fetchApi(api, "/host", POST, { json: data });
  return (response as PostHostResponse).host;
}

export async function apiDeleteHost(api: Api, query: DeleteHostQuery): Promise<void> {
  await fetchApi(api, "/host", DELETE, { query, response: "ignore" });
}

export async function apiPostPair(api: Api, request: PostPairRequest): Promise<StreamedJsonResponse<PostPairResponse1, PostPairResponse2>> {
  return fetchApi<PostPairResponse1, PostPairResponse2>(api, "/pair", POST, {
    json: request,
    response: "jsonStreaming",
    noTimeout: true,
  });
}

export async function apiWakeUp(api: Api, request: PostWakeUpRequest): Promise<void> {
  await fetchApi(api, "/host/wake", POST, {
    json: request,
    response: "ignore",
  });
}

export async function apiGetApps(api: Api, query: GetAppsQuery): Promise<Array<App>> {
  const response = (await fetchApi(api, "/apps", GET, { query })) as GetAppsResponse;
  return response.apps;
}

export async function apiGetAppImage(api: Api, query: GetAppImageQuery): Promise<Blob> {
  const response = await fetchApi(
    api,
    "/app/image",
    GET,
    {
      query,
      response: "ignore",
    },
    60000,
  );

  return response.blob();
}

export async function apiHostCancel(api: Api, request: PostCancelRequest): Promise<PostCancelResponse> {
  const response = await fetchApi(api, "/host/cancel", POST, {
    json: request,
  });

  return response as PostCancelResponse;
}
