# 在服务器上部署向量检索（embedding）服务

Nova 的「语义检索」把模型放在**外置 embedding 服务**里，主程序不内置任何推理依赖、不增加体积。
因此你可以把模型部署在一台服务器上，**客户端只需要在设置里填一个地址**，本地无需安装/下载任何模型。

协议很简单：只要是 **OpenAI 兼容** 的 `POST {地址}/v1/embeddings` 服务即可。
连不上或没配置时，Nova 会自动回退到内置的 BM25 关键词检索，员工记忆检索始终可用。

---

## 一、客户端配置（每台电脑只做一次）

打开 Nova：`设置 → 语义检索`，填三项：

- **启用语义检索**：勾选。
- **服务地址**：填服务器的 base 地址，**不要带 `/v1`**。例如 `http://192.168.1.10:11434` 或 `https://embed.your-company.com`。
- **模型名**：见下面各方案（默认 `bge-m3`）。
- **API Key**：本地/内网服务留空；网关加了鉴权就填对应 key。

填好点 **测试连接**，显示「连接成功，向量维度 N」即可。

> 注意：`下载模型（Ollama）` 按钮只对 Ollama 有效（走 `/api/pull`）。用 TEI / vLLM 时模型在启动服务时就加载好了，不需要点它。
> 换了 embedding 模型后，Nova 会自动作废旧向量、下次检索时按新模型惰性重建，无需手动操作。

---

## 二、服务器部署（三选一）

推荐用「快速小模型」做向量检索，速度快、显存/内存占用低。中文/多语言优先选 `bge-m3` 或 `bge-small-zh-v1.5`。

### 方案 A：Ollama（最简单，CPU 也能跑）

安装后让它监听所有网卡，然后拉模型：

```bash
# 安装：https://ollama.com/download （Linux 一键脚本）
curl -fsSL https://ollama.com/install.sh | sh

# 让服务对外可访问（默认只听 127.0.0.1）
export OLLAMA_HOST=0.0.0.0:11434
ollama serve &

# 拉一个 embedding 小模型（任选其一）
ollama pull bge-m3            # 中文/多语言，质量优先（默认）
ollama pull nomic-embed-text  # 更轻量、更快
```

用 Docker 亦可：

```bash
docker run -d --name ollama -p 11434:11434 \
  -v ollama:/root/.ollama \
  -e OLLAMA_HOST=0.0.0.0:11434 \
  ollama/ollama
docker exec ollama ollama pull bge-m3
```

客户端填：
- 服务地址：`http://<服务器IP>:11434`
- 模型名：`bge-m3`（或 `nomic-embed-text`）

### 方案 B：HuggingFace TEI（text-embeddings-inference，吞吐高，推荐生产）

TEI 是专门的 embedding 推理服务，单模型、启动即加载，暴露 OpenAI 兼容的 `/v1/embeddings`。

CPU：

```bash
docker run -d --name tei -p 8080:80 \
  -v tei-data:/data \
  ghcr.io/huggingface/text-embeddings-inference:cpu-latest \
  --model-id BAAI/bge-small-zh-v1.5
```

GPU（NVIDIA）：

```bash
docker run -d --name tei --gpus all -p 8080:80 \
  -v tei-data:/data \
  ghcr.io/huggingface/text-embeddings-inference:latest \
  --model-id BAAI/bge-m3
```

常用「快速小模型」：`BAAI/bge-small-zh-v1.5`（中文、~24M、512 维）、`BAAI/bge-m3`（多语言、质量更好）。

客户端填：
- 服务地址：`http://<服务器IP>:8080`
- 模型名：随意填（TEI 是单模型服务，会忽略该字段），建议写成模型 id 便于辨识，如 `bge-small-zh-v1.5`。

### 方案 C：vLLM（已有 vLLM 基建时复用）

```bash
docker run -d --name vllm-embed --gpus all -p 8000:8000 \
  vllm/vllm-openai:latest \
  --model BAAI/bge-m3
```

客户端填：
- 服务地址：`http://<服务器IP>:8000`
- 模型名：**必须**与启动时的 `--model` 一致，即 `BAAI/bge-m3`。

---

## 三、校验服务是否正常

任意机器上执行（把地址/模型名换成你的）：

```bash
curl http://<服务器IP>:11434/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{"model":"bge-m3","input":["你好，向量检索"]}'
```

返回里有 `data[0].embedding` 数组即成功。也可以直接在 Nova 里点 **测试连接**。

---

## 四、进阶：加鉴权 / HTTPS（可选）

内网直连一般无需鉴权。若要对公网暴露，建议前置 Nginx/Caddy 做 HTTPS + Bearer 鉴权：

```nginx
server {
  listen 443 ssl;
  server_name embed.your-company.com;
  # ssl_certificate ...; ssl_certificate_key ...;
  location /v1/ {
    if ($http_authorization != "Bearer YOUR_SECRET") { return 401; }
    proxy_pass http://127.0.0.1:11434;
  }
}
```

客户端此时填：
- 服务地址：`https://embed.your-company.com`
- API Key：`YOUR_SECRET`

---

## 五、常见问题

- **测试连接失败 / 连不上**：确认服务监听 `0.0.0.0` 而非 `127.0.0.1`；确认服务器防火墙放行端口；`curl` 能通再回 Nova 测。
- **维度显示为 0 或报错**：模型没加载好（Ollama 未 `pull`；TEI/vLLM 的 `--model` 写错）。
- **想换更快/更小的模型**：换模型名后 Nova 自动重建向量；无需清理数据。
- **完全不想部署**：不勾选语义检索即可，Nova 自动用内置 BM25 关键词检索，员工记忆/知识检索照常工作。
