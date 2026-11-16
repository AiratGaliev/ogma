/**
 * Adapted syntax-only consumer fixture from Codogma Engine's AI chat service.
 */

import { createServer } from "node:http"
import ZAI from "z-ai-web-dev-sdk"

const PORT = 3031
let zaiPromise = null

function getZai() {
    if (!zaiPromise) {
        zaiPromise = ZAI.create()
    }
    return zaiPromise
}

function json(res, body, status = 200) {
    res.writeHead(status, { "Content-Type": "application/json" })
    res.end(JSON.stringify(body))
}

function readBody(req) {
    return new Promise((resolve, reject) => {
        let data = ""
        req.on("data", chunk => data += chunk)
        req.on("end", () => resolve(data))
        req.on("error", reject)
    })
}

createServer(async (req, res) => {
    try {
        const url = new URL(req.url, `http://localhost:${PORT}`)
        if (req.method === "GET" && url.pathname === "/health") {
            json(res, { ok: true, service: "codogma-ai-chat" })
            return
        }
        if (req.method === "POST" && url.pathname === "/chat") {
            try {
                const body = JSON.parse(await readBody(req))
                const messages = Array.isArray(body.messages) ? body.messages : []
                if (messages.length === 0) {
                    json(res, { error: "messages array is required" }, 400)
                    return
                }
                const zai = await getZai()
                const completion = await zai.chat.completions.create({
                    messages,
                    thinking: { type: "disabled" }
                })
                const text = completion?.choices?.[0]?.message?.content ?? ""
                json(res, { text })
            } catch (error) {
                console.error("chat error:", error?.message || error)
                json(res, { error: String(error?.message || error) }, 500)
            }
            return
        }
        json(res, { error: "not found" }, 404)
    } catch (error) {
        console.error("request error:", error?.message || error)
        json(res, { error: String(error?.message || error) }, 500)
    }
}).listen(PORT, () => console.log(`codogma-ai-chat listening on ${PORT}`))
