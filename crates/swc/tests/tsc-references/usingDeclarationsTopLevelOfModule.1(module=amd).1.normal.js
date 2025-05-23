//// [usingDeclarationsTopLevelOfModule.1.ts]
define([
    "require",
    "exports",
    "@swc/helpers/_/_ts_add_disposable_resource",
    "@swc/helpers/_/_ts_dispose_resources"
], function(require, exports, _ts_add_disposable_resource, _ts_dispose_resources) {
    "use strict";
    Object.defineProperty(exports, "__esModule", {
        value: true
    });
    function _export(target, all) {
        for(var name in all)Object.defineProperty(target, name, {
            enumerable: true,
            get: Object.getOwnPropertyDescriptor(all, name).get
        });
    }
    _export(exports, {
        get default () {
            return _default;
        },
        get w () {
            return w;
        },
        get x () {
            return x;
        },
        get y () {
            return y;
        }
    });
    const env = {
        stack: [],
        error: void 0,
        hasError: false
    };
    try {
        var z = _ts_add_disposable_resource._(env, {
            [Symbol.dispose] () {}
        }, false);
        var y = 2;
        console.log(w, x, y, z);
    } catch (e) {
        env.error = e;
        env.hasError = true;
    } finally{
        _ts_dispose_resources._(env);
    }
    const x = 1;
    const w = 3;
    const _default = 4;
});
