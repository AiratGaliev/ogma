async function* produce(values) {
    for (const value of values) {
        await Promise.resolve();
        yield value * 2;
    }
}

async function collect(input) {
    const results = [];
    for await (const value of produce(input)) {
        results.push(value);
    }
    return results;
}

void collect([1, 2, 3]);
