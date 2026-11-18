const keyboardKeys = ["A", "D", "ArrowLeft", "ArrowRight", "Space"];
const keyboardIds = [0, 1, 2, 3, 4];
const gamepadButtons = ["South", "DPadLeft", "DPadRight"];
const gamepadIds = [0, 1, 2];

globalThis.runScalar = function runScalar() {
    const frameStart = engine.snapshot.frameId();
    let checksum = engine.time.deltaSeconds();

    for (let index = 0; index < keyboardKeys.length; index += 1) {
        if (engine.input.isDown(keyboardKeys[index])) checksum += 1;
        if (engine.input.wasPressed(keyboardKeys[index])) checksum += 1;
    }
    for (let index = 0; index < gamepadButtons.length; index += 1) {
        if (engine.input.gamepad.isDown(gamepadButtons[index])) checksum += 1;
        if (engine.input.gamepad.wasPressed(gamepadButtons[index])) checksum += 1;
    }
    if (engine.input.gamepad.connected()) checksum += 1;
    checksum += engine.input.gamepad.leftStickX();

    for (let index = 0; index < entityIds.length; index += 1) {
        const entityId = entityIds[index];
        checksum += engine.entities.getPositionX(entityId) ?? 0;
        checksum += engine.entities.getPositionY(entityId) ?? 0;
    }

    const collisionCount = engine.physics.collisionCount();
    for (let index = 0; index < collisionCount; index += 1) {
        checksum += engine.physics.collisionEntityA(index);
        checksum += engine.physics.collisionEntityB(index);
    }

    if (frameStart !== engine.snapshot.frameId()) {
        throw new Error("FrameSnapshot identity changed during a synchronous update");
    }
    return checksum;
};

globalThis.runBatchSet = function runBatchSet() {
    const frameStart = engine.snapshot.frameId();
    let checksum = engine.time.deltaSeconds();

    for (let index = 0; index < keyboardIds.length; index += 1) {
        if (engine.input.isDownId(keyboardIds[index])) checksum += 1;
        if (engine.input.wasPressedId(keyboardIds[index])) checksum += 1;
    }
    for (let index = 0; index < gamepadIds.length; index += 1) {
        if (engine.input.gamepad.isDownId(gamepadIds[index])) checksum += 1;
        if (engine.input.gamepad.wasPressedId(gamepadIds[index])) checksum += 1;
    }
    if (engine.input.gamepad.connected()) checksum += 1;
    checksum += engine.input.gamepad.leftStickX();

    engine.entities.readPositionsSet(entityIds, positionOutput);
    for (let index = 0; index < positionOutput.length; index += 1) {
        checksum += positionOutput[index];
    }

    engine.physics.readCollisionsSet(collisionOutput);
    for (let index = 0; index < collisionOutput.length; index += 1) {
        checksum += collisionOutput[index];
    }

    if (frameStart !== engine.snapshot.frameId()) {
        throw new Error("FrameSnapshot identity changed during a synchronous update");
    }
    return checksum;
};

globalThis.runBatchBulk = function runBatchBulk() {
    const frameStart = engine.snapshot.frameId();
    let checksum = engine.time.deltaSeconds();

    for (let index = 0; index < keyboardIds.length; index += 1) {
        if (engine.input.isDownId(keyboardIds[index])) checksum += 1;
        if (engine.input.wasPressedId(keyboardIds[index])) checksum += 1;
    }
    for (let index = 0; index < gamepadIds.length; index += 1) {
        if (engine.input.gamepad.isDownId(gamepadIds[index])) checksum += 1;
        if (engine.input.gamepad.wasPressedId(gamepadIds[index])) checksum += 1;
    }
    if (engine.input.gamepad.connected()) checksum += 1;
    checksum += engine.input.gamepad.leftStickX();

    engine.entities.readPositionsBulk(entityIds, positionOutput);
    for (let index = 0; index < positionOutput.length; index += 1) {
        checksum += positionOutput[index];
    }

    engine.physics.readCollisionsBulk(collisionOutput);
    for (let index = 0; index < collisionOutput.length; index += 1) {
        checksum += collisionOutput[index];
    }

    if (frameStart !== engine.snapshot.frameId()) {
        throw new Error("FrameSnapshot identity changed during a synchronous update");
    }
    return checksum;
};

globalThis.runPureJavaScript = function runPureJavaScript() {
    let checksum = deltaSecondsValue;
    for (let index = 0; index < pureKeyDown.length; index += 1) {
        if (pureKeyDown[index] !== 0) checksum += 1;
        if (pureKeyPressed[index] !== 0) checksum += 1;
    }
    for (let index = 0; index < pureGamepadDown.length; index += 1) {
        if (pureGamepadDown[index] !== 0) checksum += 1;
        if (pureGamepadPressed[index] !== 0) checksum += 1;
    }
    if (pureGamepadConnectedValue) checksum += 1;
    checksum += pureGamepadLeftStickXValue;
    for (let index = 0; index < positionOutput.length; index += 1) {
        checksum += positionOutput[index];
    }
    for (let index = 0; index < collisionOutput.length; index += 1) {
        checksum += collisionOutput[index];
    }
    return checksum;
};

// Reproduction of the pinned CGE quickjs_host_api/frame_submit_and_update workload:
// Array-based IDs, a reusable Float32Array, eight "A" lookups, position summation,
// and deltaSeconds. The adjacent variants change one boundary at a time.
const cgeEntityIds = Array.from(entityIds);
const cgeEntityIdsTyped = new Uint32Array(cgeEntityIds);
const cgePositionsOutput = new Float32Array(cgeEntityIds.length * 2);

globalThis.runCgeScalar = function runCgeScalar() {
    let checksum = 0;
    for (let index = 0; index < cgeEntityIds.length; index += 1) {
        const entityId = cgeEntityIds[index];
        checksum += engine.entities.getPositionX(entityId) ?? 0;
        checksum += engine.entities.getPositionY(entityId) ?? 0;
    }
    for (let index = 0; index < 8; index += 1) {
        if (engine.input.isDown("A")) checksum += 1;
    }
    return checksum + engine.time.deltaSeconds();
};

globalThis.runCgeReadPositions = function runCgeReadPositions() {
    engine.entities.readPositions(cgeEntityIds, cgePositionsOutput);
    let checksum = 0;
    for (let index = 0; index < cgePositionsOutput.length; index += 1) {
        checksum += cgePositionsOutput[index];
    }
    for (let index = 0; index < 8; index += 1) {
        if (engine.input.isDown("A")) checksum += 1;
    }
    return checksum + engine.time.deltaSeconds();
};

globalThis.runCgeNumericReadPositions = function runCgeNumericReadPositions() {
    engine.entities.readPositions(cgeEntityIds, cgePositionsOutput);
    let checksum = 0;
    for (let index = 0; index < cgePositionsOutput.length; index += 1) {
        checksum += cgePositionsOutput[index];
    }
    for (let index = 0; index < 8; index += 1) {
        if (engine.input.isDownId(0)) checksum += 1;
    }
    return checksum + engine.time.deltaSeconds();
};

globalThis.runCgeBulk = function runCgeBulk() {
    engine.entities.readPositionsBulk(cgeEntityIdsTyped, cgePositionsOutput);
    let checksum = 0;
    for (let index = 0; index < cgePositionsOutput.length; index += 1) {
        checksum += cgePositionsOutput[index];
    }
    for (let index = 0; index < 8; index += 1) {
        if (engine.input.isDownId(0)) checksum += 1;
    }
    return checksum + engine.time.deltaSeconds();
};

globalThis.runCgePureJavaScript = function runCgePureJavaScript() {
    let checksum = 0;
    for (let index = 0; index < cgePositionsOutput.length; index += 1) {
        checksum += cgePositionsOutput[index];
    }
    for (let index = 0; index < 8; index += 1) {
        if (pureCgeKeyDownValue) checksum += 1;
    }
    return checksum + deltaSecondsValue;
};
