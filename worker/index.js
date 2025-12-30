export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    // 匹配 API 或健康检查
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

    // 1. 彻底处理 CORS 预检
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

    // 2. 干净的 Header 转发策略
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

    // 3. 针对 POST 请求的关键修复：确保 Content-Type 存在
    if (request.method === "POST" && !cleanHeaders.has("content-type")) {
      cleanHeaders.set("content-type", "application/json");
    }

    // 4. 发起 fetch，明确指定不使用 Cloudflare 的代理特性
    const response = await fetch(targetUrl, {
      method: request.method,
      headers: cleanHeaders,
      body:
        request.method !== "GET" && request.method !== "HEAD"
          ? await request.arrayBuffer()
          : undefined,
      redirect: "follow",
    });

    // 5. 包装响应
    const modifiedResponse = new Response(response.body, {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });

    // 强制覆盖跨域头，防止后端没给
    modifiedResponse.headers.set("Access-Control-Allow-Origin", "*");

    return modifiedResponse;
  } catch (e) {
    // 如果出错，返回错误详情以便调试
    return new Response(JSON.stringify({ error: e.message, stack: e.stack }), {
      status: 502,
      headers: { "Content-Type": "application/json" },
    });
  }
}
