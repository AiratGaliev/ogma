import { readFile } from "node:fs/promises";

export const loadText = (path) => readFile(path, "utf8");

export default async function start(path) {
    const text = await loadText(path);
    return { path, text };
}

export const moduleUrl = import.meta.url;
