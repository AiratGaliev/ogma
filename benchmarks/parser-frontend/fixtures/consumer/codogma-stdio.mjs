#!/usr/bin/env node
/**
 * Adapted syntax-only consumer fixture from Codogma Engine's MCP stdio launcher.
 */

import readline from "node:readline"

const BRIDGE_URL = process.env.PHASER_EDITOR_URL
    || `http://127.0.0.1:${process.env.PORT || 3000}/editor/mcp-bridge`

const SERVER_INFO = {
    name: "phaser-editor-mcp",
    title: "Codogma Engine MCP Server",
    version: "1.0.0"
}

let toolsCache = null

async function postBridge(body) {
    try {
        const response = await fetch(BRIDGE_URL, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(body)
        })
        return await response.json()
    } catch (error) {
        return {
            error: `Could not connect to Codogma Engine at ${BRIDGE_URL}.`,
            details: error.message
        }
    }
}

async function listTools() {
    if (toolsCache) return toolsCache
    const result = await postBridge({ tool: "__tools_list__", args: {} })
    toolsCache = result.tools || []
    return toolsCache
}

function writeMessage(message) {
    process.stdout.write(JSON.stringify(message) + "\n")
}

function reply(id, result) {
    writeMessage({ jsonrpc: "2.0", id, result })
}

function replyError(id, code, message) {
    writeMessage({ jsonrpc: "2.0", id, error: { code, message } })
}

const rl = readline.createInterface({ input: process.stdin, terminal: false })
rl.on("line", async (line) => {
    const trimmed = line.trim()
    if (!trimmed) return
    let message
    try {
        message = JSON.parse(trimmed)
    } catch (error) {
        return
    }
    const { id, method, params } = message
    const isNotification = id === undefined || id === null
    try {
        switch (method) {
            case "initialize":
                reply(id, {
                    protocolVersion: params?.protocolVersion || "2024-11-05",
                    capabilities: { tools: {} },
                    serverInfo: SERVER_INFO
                })
                break
            case "notifications/initialized":
                break
            case "ping":
                reply(id, {})
                break
            case "tools/list":
                reply(id, { tools: await listTools() })
                break
            case "tools/call": {
                const name = params?.name
                const args = params?.arguments || {}
                const result = await postBridge({ tool: name, args })
                if (result.error) {
                    reply(id, {
                        content: [{ type: "text", text: `Tool '${name}' error: ${result.error}` }],
                        isError: true
                    })
                } else {
                    reply(id, result)
                }
                break
            }
            default:
                if (!isNotification) replyError(id, -32601, `Method not found: ${method}`)
        }
    } catch (error) {
        if (!isNotification) replyError(id, -32603, String(error.message || error))
    }
})

process.stderr.write(`Codogma Engine MCP server -> ${BRIDGE_URL}\n`)
