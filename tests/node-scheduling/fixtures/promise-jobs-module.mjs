export let trace = "";

export const first = Promise.resolve().then(function () {
    trace = trace + "first,";
    return Promise.resolve().then(function () {
        trace = trace + "nested,";
    });
});

export const second = Promise.resolve().then(function () {
    trace = trace + "second,";
});
