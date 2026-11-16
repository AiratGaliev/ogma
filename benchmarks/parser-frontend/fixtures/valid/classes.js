class Base {
    value() {
        return 1;
    }
}

class Counter extends Base {
    #count = 0;
    static created = 0;

    static {
        this.created += 1;
    }

    increment(step = 1) {
        return (this.#count += step);
    }

    get count() {
        return this.#count;
    }
}

const counter = new Counter();
counter.increment();
void counter.count;
