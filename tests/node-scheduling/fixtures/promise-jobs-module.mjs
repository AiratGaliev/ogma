export let trace = "";

function schedule() {
    Promise.resolve().then(function () {
        trace = trace + "first,";
        return Promise.resolve().then(function () {
            trace = trace + "nested,";
        });
    });

    Promise.resolve().then(function () {
        trace = trace + "second,";
    });
}

schedule();
