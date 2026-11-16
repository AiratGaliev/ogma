// Adapted syntax-only CommonJS consumer fixture from Codogma Engine.
const fs = require("fs")
const process = require("process")

const phaserPath = process.env.PHASER_PATH
const phaserJsonPath = phaserPath + "/phaser3-docs/json/phaser.json"
const spineJsonPath = phaserPath + "/spine-phaser-docs/out.json"
const docsMap = {}

for (const inputJsonFile of [phaserJsonPath, spineJsonPath]) {
    const content = fs.readFileSync(inputJsonFile)
    const data = JSON.parse(content.toString())
    for (const item of data.docs) {
        let longname = item.longname.replace("#", ".")
        if (item.meta && item.meta.path
            && (item.meta.path.endsWith("spine-phaser-docs/spine-core/dist")
                || item.meta.path.endsWith("spine-phaser-docs/spine-phaser/dist"))) {
            longname = "spine." + longname
        }
        docsMap[longname] = item.description || item.classdesc
        if (item.params) {
            for (const param of item.params) {
                docsMap[longname + "(" + param.name + ")"] = param.description
            }
        } else if (item.properties) {
            for (const prop of item.properties) {
                docsMap[longname + "." + prop.name] = prop.description
            }
        }
    }
}

function makeHelpFile(members, outputPath) {
    const outputMap = {}
    for (const name of members) {
        const docs = docsMap[name]
        if (!docs) throw new Error("Cannot find name: " + name)
        outputMap[name] = docs
    }
    fs.writeFileSync(outputPath, JSON.stringify(outputMap, null, 2))
}

module.exports = { makeHelpFile }
