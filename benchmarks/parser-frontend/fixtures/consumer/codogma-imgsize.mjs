/**
 * Adapted syntax-only consumer fixture from Codogma Engine's image-size helper.
 */

export function parseImageSize(buf) {
    if (!buf || buf.length < 8) return null
    if (buf[0] === 0x89 && buf[1] === 0x50 && buf[2] === 0x4e && buf[3] === 0x47) {
        if (buf.length >= 24) return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20) }
        return null
    }
    if (buf[0] === 0x47 && buf[1] === 0x49 && buf[2] === 0x46) {
        if (buf.length >= 10) return { width: buf.readUInt16LE(6), height: buf.readUInt16LE(8) }
        return null
    }
    if (buf[0] === 0x42 && buf[1] === 0x4d) {
        if (buf.length >= 26) return { width: Math.abs(buf.readInt32LE(18)), height: Math.abs(buf.readInt32LE(22)) }
        return null
    }
    if (buf.length >= 30
        && buf.toString("ascii", 0, 4) === "RIFF"
        && buf.toString("ascii", 8, 12) === "WEBP") {
        const format = buf.toString("ascii", 12, 16)
        if (format === "VP8 ") return { width: buf.readUInt16LE(26) & 0x3fff, height: buf.readUInt16LE(28) & 0x3fff }
        if (format === "VP8L") {
            const bits = buf.readUInt32LE(21)
            return { width: (bits & 0x3fff) + 1, height: ((bits >> 14) & 0x3fff) + 1 }
        }
        if (format === "VP8X") {
            const width = 1 + (buf[24] | (buf[25] << 8) | (buf[26] << 16))
            const height = 1 + (buf[27] | (buf[28] << 8) | (buf[29] << 16))
            return { width, height }
        }
        return null
    }
    if (buf[0] === 0xff && buf[1] === 0xd8) {
        let offset = 2
        while (offset + 9 < buf.length) {
            if (buf[offset] !== 0xff) {
                offset++
                continue
            }
            const marker = buf[offset + 1]
            if ((marker >= 0xc0 && marker <= 0xcf) && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
                return { height: buf.readUInt16BE(offset + 5), width: buf.readUInt16BE(offset + 7) }
            }
            const size = buf.readUInt16BE(offset + 2)
            offset += 2 + size
        }
        return null
    }
    const head = buf.toString("utf8", 0, Math.min(buf.length, 1024)).toLowerCase()
    if (head.includes("<svg")) {
        const width = head.match(/width\s*=\s*["']?([\d.]+)/)
        const height = head.match(/height\s*=\s*["']?([\d.]+)/)
        const viewBox = head.match(/viewbox\s*=\s*["']\s*[\d.-]+\s+[\d.-]+\s+([\d.]+)\s+([\d.]+)/)
        if (width && height) return { width: Math.round(parseFloat(width[1])), height: Math.round(parseFloat(height[1])) }
        if (viewBox) return { width: Math.round(parseFloat(viewBox[1])), height: Math.round(parseFloat(viewBox[2])) }
        return { width: 300, height: 150 }
    }
    return null
}
