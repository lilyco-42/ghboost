# Cloudflare Workers VLESS Proxy

纯 serverless 代理边缘节点，基于 [edgetunnel](https://github.com/zizifn/edgetunnel) 部署在 Cloudflare Workers 上。

## 信息

| 项目 | 值 |
|------|-----|
| Worker 地址 | `ghboost-vless-proxy.m15178682641.workers.dev` |
| UUID | `25934eef-2683-4954-a7c3-57a2621c4a9e` |
| 协议 | VLESS + WebSocket + TLS |
| 端口 | 443 |
| SNI | `ghboost-vless-proxy.m15178682641.workers.dev` |

## VLESS URI

```
vless://25934eef-2683-4954-a7c3-57a2621c4a9e@ghboost-vless-proxy.m15178682641.workers.dev:443?encryption=none&security=tls&sni=ghboost-vless-proxy.m15178682641.workers.dev&fp=randomized&type=ws&host=ghboost-vless-proxy.m15178682641.workers.dev&path=%2F%3Fed%3D2048#ghboost-vless-proxy
```

## Clash-meta 配置

```yaml
- type: vless
  name: ghboost-vless-proxy
  server: ghboost-vless-proxy.m15178682641.workers.dev
  port: 443
  uuid: 25934eef-2683-4954-a7c3-57a2621c4a9e
  network: ws
  tls: true
  udp: false
  sni: ghboost-vless-proxy.m15178682641.workers.dev
  client-fingerprint: chrome
  ws-opts:
    path: "/?ed=2048"
    headers:
      host: ghboost-vless-proxy.m15178682641.workers.dev
```

## 部署命令

```bash
cd edgetunnel
npm install
wrangler deploy
```

## 测试结果

- GitHub: 200 (698ms) ✅
- Cloudflare: 200 (591ms) ✅
- Google: FAIL (VLESS-WS TLS 握手问题，不影响其他站点)

## 注意事项

- 免费版每天 100K 请求
- 如需自定义域名，在 Cloudflare 控制台添加路由
- 更新 UUID: 修改 `wrangler.toml` 中的 `UUID` 环境变量，然后 `wrangler deploy`
