export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    // Worker 只托管静态前端；API 和健康检查转发到 Rust 后端
    if (url.pathname.startsWith("/api/") || url.pathname === "/health") {
      return handleAPIRequest(request, env.BACKEND_URL);
    }
    return env.ASSETS.fetch(request);
  },
};

async function handleAPIRequest(request, backendURL) {
  try {
    const url = new URL(request.url);
    const targetBase = new URL(backendURL);
    const targetUrl = `${targetBase.origin}${url.pathname}${url.search}`;

    // 预检请求由 Worker 直接返回，避免后端部署差异影响跨域
    if (request.method === "OPTIONS") {
      return new Response(null, {
        status: 204,
        headers: {
          "Access-Control-Allow-Origin": "*",
          "Access-Control-Allow-Methods": "GET, POST, PUT, DELETE, OPTIONS",
          "Access-Control-Allow-Headers": "*",
          "Access-Control-Max-Age": "86400",
        },
      });
    }

    // Cloudflare 注入的连接和来源头不转发，后端只接收业务相关 header
    const cleanHeaders = new Headers();
    const forbiddenHeaders = [
      "host",
      "cf-ray",
      "cf-connecting-ip",
      "cf-visitor",
      "x-forwarded-for",
      "x-real-ip",
      "connection",
      "adventure",
    ];

    for (const [key, value] of request.headers.entries()) {
      if (
        !forbiddenHeaders.includes(key.toLowerCase()) &&
        !key.toLowerCase().startsWith("cf-")
      ) {
        cleanHeaders.set(key, value);
      }
    }

    // 部分静态前端请求没有显式 Content-Type，Axum JSON extractor 需要该值
    if (request.method === "POST" && !cleanHeaders.has("content-type")) {
      cleanHeaders.set("content-type", "application/json");
    }

    const response = await fetch(targetUrl, {
      method: request.method,
      headers: cleanHeaders,
      body:
        request.method !== "GET" && request.method !== "HEAD"
          ? await request.arrayBuffer()
          : undefined,
      redirect: "follow",
    });

    const modifiedResponse = new Response(response.body, {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });

    // 静态前端和后端可能分域部署，跨域头在边缘层统一补齐
    modifiedResponse.headers.set("Access-Control-Allow-Origin", "*");

    return modifiedResponse;
  } catch (e) {
    return new Response(JSON.stringify({ error: e.message, stack: e.stack }), {
      status: 502,
      headers: { "Content-Type": "application/json" },
    });
  }
}
