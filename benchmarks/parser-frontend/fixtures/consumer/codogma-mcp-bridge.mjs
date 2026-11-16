/**
 * Adapted syntax-only consumer fixture from Codogma Engine's MCP bridge.
 */

import { createServer } from "node:http"
import { WebSocketServer } from "ws"

const PORT = Number(process.env["PORT"] || 1902)
const TOOL_TIMEOUT_MS = 30_000
const clients = new Set()
const pending = new Map()
const activity = []

function log(text) {
    activity.unshift({ time: new Date().toISOString(), text })
    if (activity.length > 50) activity.pop()
}

function corsHeaders() {
    return {
        "Access-Control-Allow-Origin": "*",
        "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
        "Access-Control-Allow-Headers": "Content-Type",
    }
}

function json(res, body, status = 200) {
    const data = JSON.stringify(body)
    res.writeHead(status, { "Content-Type": "application/json", ...corsHeaders() })
    res.end(data)
}

function readBody(req) {
    return new Promise((resolve, reject) => {
        let data = ""
        req.on("data", chunk => data += chunk)
        req.on("end", () => resolve(data))
        req.on("error", reject)
    })
}

function sendToEditors(message) {
    const data = JSON.stringify(message)
    for (const ws of clients) {
        try {
            ws.send(data)
        } catch (error) {
            console.error("bridge send failure", error)
        }
    }
}

const server = createServer(async (req, res) => {
    if (req.method === "OPTIONS") {
        res.writeHead(204, corsHeaders())
        res.end()
        return
    }
    if (req.method === "GET") {
        json(res, { status: "ok", clients: clients.size, pending: pending.size, port: PORT, activity })
        return
    }
    if (req.method === "POST") {
        let body
        try {
            body = JSON.parse(await readBody(req))
        } catch (error) {
            json(res, { error: "Invalid JSON body." }, 400)
            return
        }
        const tool = body?.tool
        const args = body?.args ?? {}
        if (typeof tool !== "string" || tool.length === 0) {
            json(res, { error: "Missing tool." }, 400)
            return
        }
        if (clients.size === 0) {
            log(`POST ${tool} rejected`)
            json(res, { error: "No editor client is connected." }, 503)
            return
        }
        const id = `req-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
        const result = await new Promise((resolve) => {
            const timer = setTimeout(() => {
                pending.delete(id)
                resolve({ error: `Tool '${tool}' timed out after ${TOOL_TIMEOUT_MS / 1000}s.` })
            }, TOOL_TIMEOUT_MS)
            pending.set(id, { id, tool, resolve, timer })
            sendToEditors({ id, tool, args })
        })
        clearTimeout(pending.get(id)?.timer)
        pending.delete(id)
        json(res, result)
        return
    }
    json(res, { error: `Unsupported method ${req.method}.` }, 405)
})

const wss = new WebSocketServer({ server })
wss.on("connection", (ws) => {
    clients.add(ws)
    log("editor client connected")
    ws.on("message", (data) => {
        let message
        try {
            message = JSON.parse(String(data))
        } catch (error) {
            console.error("invalid WS message", error)
            return
        }
        if (message && typeof message.id === "string" && pending.has(message.id)) {
            const request = pending.get(message.id)
            pending.delete(message.id)
            clearTimeout(request.timer)
            request.resolve(message.result ?? { error: "Editor returned no result." })
        } else if (message && message.event) {
            log(`editor event: ${message.event}`)
        }
    })
    ws.on("close", () => {
        clients.delete(ws)
        log("editor client disconnected")
    })
})

server.listen(PORT, () => console.log(`codogma-mcp-bridge listening on ${PORT}`))
