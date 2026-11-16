const target = { id: 1, name: "Ogma" };
const proxy = new Proxy(target, {
    get(object, key, receiver) {
        return Reflect.get(object, key, receiver);
    },
});

const entries = Object.entries({ ...proxy, active: true })
    .map(([key, value]) => `${key}:${String(value)}`);

const iterator = new Map(entries.map((value, index) => [index, value])).values();
void [...iterator];
