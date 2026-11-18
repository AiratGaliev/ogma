const keyboardKeys = ["A", "D", "ArrowLeft", "Space"];
const keyboardIds = [0, 1, 2, 3];
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

    for (let index = 0; index < entityIds.length; index += 1) {
        const entityId = entityIds[index];
        checksum += engine.entities.getPositionX(entityId);
        checksum += engine.entities.getPositionY(entityId);
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
    }
    for (let index = 0; index < gamepadIds.length; index += 1) {
        if (engine.input.gamepad.isDownId(gamepadIds[index])) checksum += 1;
    }

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
    for (let index = 0; index < positionOutput.length; index += 1) {
        checksum += positionOutput[index];
    }
    for (let index = 0; index < collisionOutput.length; index += 1) {
        checksum += collisionOutput[index];
    }
    return checksum;
};
