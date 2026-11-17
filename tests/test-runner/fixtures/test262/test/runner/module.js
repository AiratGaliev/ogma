/*---
description: Module tests are executed once in module mode.
features: [Array]
flags: [module]
---*/
// result: pass
import { fixtureValue } from './import_FIXTURE.js';
if (fixtureValue !== 42) throw new Error('fixture resolution failed');
