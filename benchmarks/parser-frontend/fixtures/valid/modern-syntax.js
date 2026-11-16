"use strict";

const defaults = { host: "127.0.0.1", port: 0 };
const configuration = {
    ...defaults,
    port: 3031,
    nested: { enabled: true, label: String.raw`worker-${3}` },
};

const { port, ...rest } = configuration;
const normalized = configuration.nested?.enabled ? (port ?? 0) : -1;
const values = [0, 1, ...Object.values(rest)];

function createCounter(start = 0) {
    let current = start;
    return (delta = 1) => (current += delta);
}

const counter = createCounter(normalized);
void values.map((value) => counter(Number(value)));
