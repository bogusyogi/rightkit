var Eo = { exports: {} }, Mn = {};
/**
 * @license React
 * react-jsx-runtime.production.js
 *
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */
var Rm = Symbol.for("react.transitional.element"), Bm = Symbol.for("react.fragment");
function po(l, t, e) {
  var a = null;
  if (e !== void 0 && (a = "" + e), t.key !== void 0 && (a = "" + t.key), "key" in t) {
    e = {};
    for (var u in t)
      u !== "key" && (e[u] = t[u]);
  } else e = t;
  return t = e.ref, {
    $$typeof: Rm,
    type: l,
    key: a,
    ref: t !== void 0 ? t : null,
    props: e
  };
}
Mn.Fragment = Bm;
Mn.jsx = po;
Mn.jsxs = po;
Eo.exports = Mn;
var g = Eo.exports, To = { exports: {} }, On = {}, zo = { exports: {} }, Ao = {};
/**
 * @license React
 * scheduler.production.js
 *
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */
(function(l) {
  function t(M, D) {
    var N = M.length;
    M.push(D);
    l: for (; 0 < N; ) {
      var X = N - 1 >>> 1, dl = M[X];
      if (0 < u(dl, D))
        M[X] = D, M[N] = dl, N = X;
      else break l;
    }
  }
  function e(M) {
    return M.length === 0 ? null : M[0];
  }
  function a(M) {
    if (M.length === 0) return null;
    var D = M[0], N = M.pop();
    if (N !== D) {
      M[0] = N;
      l: for (var X = 0, dl = M.length, yu = dl >>> 1; X < yu; ) {
        var vu = 2 * (X + 1) - 1, Jn = M[vu], Pt = vu + 1, gu = M[Pt];
        if (0 > u(Jn, N))
          Pt < dl && 0 > u(gu, Jn) ? (M[X] = gu, M[Pt] = N, X = Pt) : (M[X] = Jn, M[vu] = N, X = vu);
        else if (Pt < dl && 0 > u(gu, N))
          M[X] = gu, M[Pt] = N, X = Pt;
        else break l;
      }
    }
    return D;
  }
  function u(M, D) {
    var N = M.sortIndex - D.sortIndex;
    return N !== 0 ? N : M.id - D.id;
  }
  if (l.unstable_now = void 0, typeof performance == "object" && typeof performance.now == "function") {
    var n = performance;
    l.unstable_now = function() {
      return n.now();
    };
  } else {
    var i = Date, c = i.now();
    l.unstable_now = function() {
      return i.now() - c;
    };
  }
  var f = [], o = [], d = 1, y = null, h = 3, v = !1, p = !1, T = !1, E = !1, r = typeof setTimeout == "function" ? setTimeout : null, s = typeof clearTimeout == "function" ? clearTimeout : null, m = typeof setImmediate < "u" ? setImmediate : null;
  function S(M) {
    for (var D = e(o); D !== null; ) {
      if (D.callback === null) a(o);
      else if (D.startTime <= M)
        a(o), D.sortIndex = D.expirationTime, t(f, D);
      else break;
      D = e(o);
    }
  }
  function _(M) {
    if (T = !1, S(M), !p)
      if (e(f) !== null)
        p = !0, O || (O = !0, rl());
      else {
        var D = e(o);
        D !== null && El(_, D.startTime - M);
      }
  }
  var O = !1, z = -1, x = 5, j = -1;
  function U() {
    return E ? !0 : !(l.unstable_now() - j < x);
  }
  function B() {
    if (E = !1, O) {
      var M = l.unstable_now();
      j = M;
      var D = !0;
      try {
        l: {
          p = !1, T && (T = !1, s(z), z = -1), v = !0;
          var N = h;
          try {
            t: {
              for (S(M), y = e(f); y !== null && !(y.expirationTime > M && U()); ) {
                var X = y.callback;
                if (typeof X == "function") {
                  y.callback = null, h = y.priorityLevel;
                  var dl = X(
                    y.expirationTime <= M
                  );
                  if (M = l.unstable_now(), typeof dl == "function") {
                    y.callback = dl, S(M), D = !0;
                    break t;
                  }
                  y === e(f) && a(f), S(M);
                } else a(f);
                y = e(f);
              }
              if (y !== null) D = !0;
              else {
                var yu = e(o);
                yu !== null && El(
                  _,
                  yu.startTime - M
                ), D = !1;
              }
            }
            break l;
          } finally {
            y = null, h = N, v = !1;
          }
          D = void 0;
        }
      } finally {
        D ? rl() : O = !1;
      }
    }
  }
  var rl;
  if (typeof m == "function")
    rl = function() {
      m(B);
    };
  else if (typeof MessageChannel < "u") {
    var _e = new MessageChannel(), ma = _e.port2;
    _e.port1.onmessage = B, rl = function() {
      ma.postMessage(null);
    };
  } else
    rl = function() {
      r(B, 0);
    };
  function El(M, D) {
    z = r(function() {
      M(l.unstable_now());
    }, D);
  }
  l.unstable_IdlePriority = 5, l.unstable_ImmediatePriority = 1, l.unstable_LowPriority = 4, l.unstable_NormalPriority = 3, l.unstable_Profiling = null, l.unstable_UserBlockingPriority = 2, l.unstable_cancelCallback = function(M) {
    M.callback = null;
  }, l.unstable_forceFrameRate = function(M) {
    0 > M || 125 < M ? console.error(
      "forceFrameRate takes a positive int between 0 and 125, forcing frame rates higher than 125 fps is not supported"
    ) : x = 0 < M ? Math.floor(1e3 / M) : 5;
  }, l.unstable_getCurrentPriorityLevel = function() {
    return h;
  }, l.unstable_next = function(M) {
    switch (h) {
      case 1:
      case 2:
      case 3:
        var D = 3;
        break;
      default:
        D = h;
    }
    var N = h;
    h = D;
    try {
      return M();
    } finally {
      h = N;
    }
  }, l.unstable_requestPaint = function() {
    E = !0;
  }, l.unstable_runWithPriority = function(M, D) {
    switch (M) {
      case 1:
      case 2:
      case 3:
      case 4:
      case 5:
        break;
      default:
        M = 3;
    }
    var N = h;
    h = M;
    try {
      return D();
    } finally {
      h = N;
    }
  }, l.unstable_scheduleCallback = function(M, D, N) {
    var X = l.unstable_now();
    switch (typeof N == "object" && N !== null ? (N = N.delay, N = typeof N == "number" && 0 < N ? X + N : X) : N = X, M) {
      case 1:
        var dl = -1;
        break;
      case 2:
        dl = 250;
        break;
      case 5:
        dl = 1073741823;
        break;
      case 4:
        dl = 1e4;
        break;
      default:
        dl = 5e3;
    }
    return dl = N + dl, M = {
      id: d++,
      callback: D,
      priorityLevel: M,
      startTime: N,
      expirationTime: dl,
      sortIndex: -1
    }, N > X ? (M.sortIndex = N, t(o, M), e(f) === null && M === e(o) && (T ? (s(z), z = -1) : T = !0, El(_, N - X))) : (M.sortIndex = dl, t(f, M), p || v || (p = !0, O || (O = !0, rl()))), M;
  }, l.unstable_shouldYield = U, l.unstable_wrapCallback = function(M) {
    var D = h;
    return function() {
      var N = h;
      h = D;
      try {
        return M.apply(this, arguments);
      } finally {
        h = N;
      }
    };
  };
})(Ao);
zo.exports = Ao;
var Ym = zo.exports, Mo = { exports: {} }, H = {};
/**
 * @license React
 * react.production.js
 *
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */
var Nc = Symbol.for("react.transitional.element"), qm = Symbol.for("react.portal"), Gm = Symbol.for("react.fragment"), Qm = Symbol.for("react.strict_mode"), Xm = Symbol.for("react.profiler"), Lm = Symbol.for("react.consumer"), Zm = Symbol.for("react.context"), wm = Symbol.for("react.forward_ref"), Vm = Symbol.for("react.suspense"), Km = Symbol.for("react.memo"), Oo = Symbol.for("react.lazy"), Jm = Symbol.for("react.activity"), qf = Symbol.iterator;
function km(l) {
  return l === null || typeof l != "object" ? null : (l = qf && l[qf] || l["@@iterator"], typeof l == "function" ? l : null);
}
var Do = {
  isMounted: function() {
    return !1;
  },
  enqueueForceUpdate: function() {
  },
  enqueueReplaceState: function() {
  },
  enqueueSetState: function() {
  }
}, xo = Object.assign, No = {};
function ua(l, t, e) {
  this.props = l, this.context = t, this.refs = No, this.updater = e || Do;
}
ua.prototype.isReactComponent = {};
ua.prototype.setState = function(l, t) {
  if (typeof l != "object" && typeof l != "function" && l != null)
    throw Error(
      "takes an object of state variables to update or a function which returns an object of state variables."
    );
  this.updater.enqueueSetState(this, l, t, "setState");
};
ua.prototype.forceUpdate = function(l) {
  this.updater.enqueueForceUpdate(this, l, "forceUpdate");
};
function jo() {
}
jo.prototype = ua.prototype;
function jc(l, t, e) {
  this.props = l, this.context = t, this.refs = No, this.updater = e || Do;
}
var Uc = jc.prototype = new jo();
Uc.constructor = jc;
xo(Uc, ua.prototype);
Uc.isPureReactComponent = !0;
var Gf = Array.isArray;
function Ui() {
}
var I = { H: null, A: null, T: null, S: null }, Uo = Object.prototype.hasOwnProperty;
function Cc(l, t, e) {
  var a = e.ref;
  return {
    $$typeof: Nc,
    type: l,
    key: t,
    ref: a !== void 0 ? a : null,
    props: e
  };
}
function $m(l, t) {
  return Cc(l.type, t, l.props);
}
function Hc(l) {
  return typeof l == "object" && l !== null && l.$$typeof === Nc;
}
function Wm(l) {
  var t = { "=": "=0", ":": "=2" };
  return "$" + l.replace(/[=:]/g, function(e) {
    return t[e];
  });
}
var Qf = /\/+/g;
function kn(l, t) {
  return typeof l == "object" && l !== null && l.key != null ? Wm("" + l.key) : t.toString(36);
}
function Fm(l) {
  switch (l.status) {
    case "fulfilled":
      return l.value;
    case "rejected":
      throw l.reason;
    default:
      switch (typeof l.status == "string" ? l.then(Ui, Ui) : (l.status = "pending", l.then(
        function(t) {
          l.status === "pending" && (l.status = "fulfilled", l.value = t);
        },
        function(t) {
          l.status === "pending" && (l.status = "rejected", l.reason = t);
        }
      )), l.status) {
        case "fulfilled":
          return l.value;
        case "rejected":
          throw l.reason;
      }
  }
  throw l;
}
function ze(l, t, e, a, u) {
  var n = typeof l;
  (n === "undefined" || n === "boolean") && (l = null);
  var i = !1;
  if (l === null) i = !0;
  else
    switch (n) {
      case "bigint":
      case "string":
      case "number":
        i = !0;
        break;
      case "object":
        switch (l.$$typeof) {
          case Nc:
          case qm:
            i = !0;
            break;
          case Oo:
            return i = l._init, ze(
              i(l._payload),
              t,
              e,
              a,
              u
            );
        }
    }
  if (i)
    return u = u(l), i = a === "" ? "." + kn(l, 0) : a, Gf(u) ? (e = "", i != null && (e = i.replace(Qf, "$&/") + "/"), ze(u, t, e, "", function(o) {
      return o;
    })) : u != null && (Hc(u) && (u = $m(
      u,
      e + (u.key == null || l && l.key === u.key ? "" : ("" + u.key).replace(
        Qf,
        "$&/"
      ) + "/") + i
    )), t.push(u)), 1;
  i = 0;
  var c = a === "" ? "." : a + ":";
  if (Gf(l))
    for (var f = 0; f < l.length; f++)
      a = l[f], n = c + kn(a, f), i += ze(
        a,
        t,
        e,
        n,
        u
      );
  else if (f = km(l), typeof f == "function")
    for (l = f.call(l), f = 0; !(a = l.next()).done; )
      a = a.value, n = c + kn(a, f++), i += ze(
        a,
        t,
        e,
        n,
        u
      );
  else if (n === "object") {
    if (typeof l.then == "function")
      return ze(
        Fm(l),
        t,
        e,
        a,
        u
      );
    throw t = String(l), Error(
      "Objects are not valid as a React child (found: " + (t === "[object Object]" ? "object with keys {" + Object.keys(l).join(", ") + "}" : t) + "). If you meant to render a collection of children, use an array instead."
    );
  }
  return i;
}
function Su(l, t, e) {
  if (l == null) return l;
  var a = [], u = 0;
  return ze(l, a, "", "", function(n) {
    return t.call(e, n, u++);
  }), a;
}
function Im(l) {
  if (l._status === -1) {
    var t = l._result;
    t = t(), t.then(
      function(e) {
        (l._status === 0 || l._status === -1) && (l._status = 1, l._result = e);
      },
      function(e) {
        (l._status === 0 || l._status === -1) && (l._status = 2, l._result = e);
      }
    ), l._status === -1 && (l._status = 0, l._result = t);
  }
  if (l._status === 1) return l._result.default;
  throw l._result;
}
var Xf = typeof reportError == "function" ? reportError : function(l) {
  if (typeof window == "object" && typeof window.ErrorEvent == "function") {
    var t = new window.ErrorEvent("error", {
      bubbles: !0,
      cancelable: !0,
      message: typeof l == "object" && l !== null && typeof l.message == "string" ? String(l.message) : String(l),
      error: l
    });
    if (!window.dispatchEvent(t)) return;
  } else if (typeof process == "object" && typeof process.emit == "function") {
    process.emit("uncaughtException", l);
    return;
  }
  console.error(l);
}, Pm = {
  map: Su,
  forEach: function(l, t, e) {
    Su(
      l,
      function() {
        t.apply(this, arguments);
      },
      e
    );
  },
  count: function(l) {
    var t = 0;
    return Su(l, function() {
      t++;
    }), t;
  },
  toArray: function(l) {
    return Su(l, function(t) {
      return t;
    }) || [];
  },
  only: function(l) {
    if (!Hc(l))
      throw Error(
        "React.Children.only expected to receive a single React element child."
      );
    return l;
  }
};
H.Activity = Jm;
H.Children = Pm;
H.Component = ua;
H.Fragment = Gm;
H.Profiler = Xm;
H.PureComponent = jc;
H.StrictMode = Qm;
H.Suspense = Vm;
H.__CLIENT_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE = I;
H.__COMPILER_RUNTIME = {
  __proto__: null,
  c: function(l) {
    return I.H.useMemoCache(l);
  }
};
H.cache = function(l) {
  return function() {
    return l.apply(null, arguments);
  };
};
H.cacheSignal = function() {
  return null;
};
H.cloneElement = function(l, t, e) {
  if (l == null)
    throw Error(
      "The argument must be a React element, but you passed " + l + "."
    );
  var a = xo({}, l.props), u = l.key;
  if (t != null)
    for (n in t.key !== void 0 && (u = "" + t.key), t)
      !Uo.call(t, n) || n === "key" || n === "__self" || n === "__source" || n === "ref" && t.ref === void 0 || (a[n] = t[n]);
  var n = arguments.length - 2;
  if (n === 1) a.children = e;
  else if (1 < n) {
    for (var i = Array(n), c = 0; c < n; c++)
      i[c] = arguments[c + 2];
    a.children = i;
  }
  return Cc(l.type, u, a);
};
H.createContext = function(l) {
  return l = {
    $$typeof: Zm,
    _currentValue: l,
    _currentValue2: l,
    _threadCount: 0,
    Provider: null,
    Consumer: null
  }, l.Provider = l, l.Consumer = {
    $$typeof: Lm,
    _context: l
  }, l;
};
H.createElement = function(l, t, e) {
  var a, u = {}, n = null;
  if (t != null)
    for (a in t.key !== void 0 && (n = "" + t.key), t)
      Uo.call(t, a) && a !== "key" && a !== "__self" && a !== "__source" && (u[a] = t[a]);
  var i = arguments.length - 2;
  if (i === 1) u.children = e;
  else if (1 < i) {
    for (var c = Array(i), f = 0; f < i; f++)
      c[f] = arguments[f + 2];
    u.children = c;
  }
  if (l && l.defaultProps)
    for (a in i = l.defaultProps, i)
      u[a] === void 0 && (u[a] = i[a]);
  return Cc(l, n, u);
};
H.createRef = function() {
  return { current: null };
};
H.forwardRef = function(l) {
  return { $$typeof: wm, render: l };
};
H.isValidElement = Hc;
H.lazy = function(l) {
  return {
    $$typeof: Oo,
    _payload: { _status: -1, _result: l },
    _init: Im
  };
};
H.memo = function(l, t) {
  return {
    $$typeof: Km,
    type: l,
    compare: t === void 0 ? null : t
  };
};
H.startTransition = function(l) {
  var t = I.T, e = {};
  I.T = e;
  try {
    var a = l(), u = I.S;
    u !== null && u(e, a), typeof a == "object" && a !== null && typeof a.then == "function" && a.then(Ui, Xf);
  } catch (n) {
    Xf(n);
  } finally {
    t !== null && e.types !== null && (t.types = e.types), I.T = t;
  }
};
H.unstable_useCacheRefresh = function() {
  return I.H.useCacheRefresh();
};
H.use = function(l) {
  return I.H.use(l);
};
H.useActionState = function(l, t, e) {
  return I.H.useActionState(l, t, e);
};
H.useCallback = function(l, t) {
  return I.H.useCallback(l, t);
};
H.useContext = function(l) {
  return I.H.useContext(l);
};
H.useDebugValue = function() {
};
H.useDeferredValue = function(l, t) {
  return I.H.useDeferredValue(l, t);
};
H.useEffect = function(l, t) {
  return I.H.useEffect(l, t);
};
H.useEffectEvent = function(l) {
  return I.H.useEffectEvent(l);
};
H.useId = function() {
  return I.H.useId();
};
H.useImperativeHandle = function(l, t, e) {
  return I.H.useImperativeHandle(l, t, e);
};
H.useInsertionEffect = function(l, t) {
  return I.H.useInsertionEffect(l, t);
};
H.useLayoutEffect = function(l, t) {
  return I.H.useLayoutEffect(l, t);
};
H.useMemo = function(l, t) {
  return I.H.useMemo(l, t);
};
H.useOptimistic = function(l, t) {
  return I.H.useOptimistic(l, t);
};
H.useReducer = function(l, t, e) {
  return I.H.useReducer(l, t, e);
};
H.useRef = function(l) {
  return I.H.useRef(l);
};
H.useState = function(l) {
  return I.H.useState(l);
};
H.useSyncExternalStore = function(l, t, e) {
  return I.H.useSyncExternalStore(
    l,
    t,
    e
  );
};
H.useTransition = function() {
  return I.H.useTransition();
};
H.version = "19.2.7";
Mo.exports = H;
var A = Mo.exports, Co = { exports: {} }, zl = {};
/**
 * @license React
 * react-dom.production.js
 *
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */
var lh = A;
function Ho(l) {
  var t = "https://react.dev/errors/" + l;
  if (1 < arguments.length) {
    t += "?args[]=" + encodeURIComponent(arguments[1]);
    for (var e = 2; e < arguments.length; e++)
      t += "&args[]=" + encodeURIComponent(arguments[e]);
  }
  return "Minified React error #" + l + "; visit " + t + " for the full message or use the non-minified dev environment for full errors and additional helpful warnings.";
}
function Mt() {
}
var Tl = {
  d: {
    f: Mt,
    r: function() {
      throw Error(Ho(522));
    },
    D: Mt,
    C: Mt,
    L: Mt,
    m: Mt,
    X: Mt,
    S: Mt,
    M: Mt
  },
  p: 0,
  findDOMNode: null
}, th = Symbol.for("react.portal");
function eh(l, t, e) {
  var a = 3 < arguments.length && arguments[3] !== void 0 ? arguments[3] : null;
  return {
    $$typeof: th,
    key: a == null ? null : "" + a,
    children: l,
    containerInfo: t,
    implementation: e
  };
}
var Oa = lh.__CLIENT_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE;
function Dn(l, t) {
  if (l === "font") return "";
  if (typeof t == "string")
    return t === "use-credentials" ? t : "";
}
zl.__DOM_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE = Tl;
zl.createPortal = function(l, t) {
  var e = 2 < arguments.length && arguments[2] !== void 0 ? arguments[2] : null;
  if (!t || t.nodeType !== 1 && t.nodeType !== 9 && t.nodeType !== 11)
    throw Error(Ho(299));
  return eh(l, t, null, e);
};
zl.flushSync = function(l) {
  var t = Oa.T, e = Tl.p;
  try {
    if (Oa.T = null, Tl.p = 2, l) return l();
  } finally {
    Oa.T = t, Tl.p = e, Tl.d.f();
  }
};
zl.preconnect = function(l, t) {
  typeof l == "string" && (t ? (t = t.crossOrigin, t = typeof t == "string" ? t === "use-credentials" ? t : "" : void 0) : t = null, Tl.d.C(l, t));
};
zl.prefetchDNS = function(l) {
  typeof l == "string" && Tl.d.D(l);
};
zl.preinit = function(l, t) {
  if (typeof l == "string" && t && typeof t.as == "string") {
    var e = t.as, a = Dn(e, t.crossOrigin), u = typeof t.integrity == "string" ? t.integrity : void 0, n = typeof t.fetchPriority == "string" ? t.fetchPriority : void 0;
    e === "style" ? Tl.d.S(
      l,
      typeof t.precedence == "string" ? t.precedence : void 0,
      {
        crossOrigin: a,
        integrity: u,
        fetchPriority: n
      }
    ) : e === "script" && Tl.d.X(l, {
      crossOrigin: a,
      integrity: u,
      fetchPriority: n,
      nonce: typeof t.nonce == "string" ? t.nonce : void 0
    });
  }
};
zl.preinitModule = function(l, t) {
  if (typeof l == "string")
    if (typeof t == "object" && t !== null) {
      if (t.as == null || t.as === "script") {
        var e = Dn(
          t.as,
          t.crossOrigin
        );
        Tl.d.M(l, {
          crossOrigin: e,
          integrity: typeof t.integrity == "string" ? t.integrity : void 0,
          nonce: typeof t.nonce == "string" ? t.nonce : void 0
        });
      }
    } else t == null && Tl.d.M(l);
};
zl.preload = function(l, t) {
  if (typeof l == "string" && typeof t == "object" && t !== null && typeof t.as == "string") {
    var e = t.as, a = Dn(e, t.crossOrigin);
    Tl.d.L(l, e, {
      crossOrigin: a,
      integrity: typeof t.integrity == "string" ? t.integrity : void 0,
      nonce: typeof t.nonce == "string" ? t.nonce : void 0,
      type: typeof t.type == "string" ? t.type : void 0,
      fetchPriority: typeof t.fetchPriority == "string" ? t.fetchPriority : void 0,
      referrerPolicy: typeof t.referrerPolicy == "string" ? t.referrerPolicy : void 0,
      imageSrcSet: typeof t.imageSrcSet == "string" ? t.imageSrcSet : void 0,
      imageSizes: typeof t.imageSizes == "string" ? t.imageSizes : void 0,
      media: typeof t.media == "string" ? t.media : void 0
    });
  }
};
zl.preloadModule = function(l, t) {
  if (typeof l == "string")
    if (t) {
      var e = Dn(t.as, t.crossOrigin);
      Tl.d.m(l, {
        as: typeof t.as == "string" && t.as !== "script" ? t.as : void 0,
        crossOrigin: e,
        integrity: typeof t.integrity == "string" ? t.integrity : void 0
      });
    } else Tl.d.m(l);
};
zl.requestFormReset = function(l) {
  Tl.d.r(l);
};
zl.unstable_batchedUpdates = function(l, t) {
  return l(t);
};
zl.useFormState = function(l, t, e) {
  return Oa.H.useFormState(l, t, e);
};
zl.useFormStatus = function() {
  return Oa.H.useHostTransitionStatus();
};
zl.version = "19.2.7";
function Ro() {
  if (!(typeof __REACT_DEVTOOLS_GLOBAL_HOOK__ > "u" || typeof __REACT_DEVTOOLS_GLOBAL_HOOK__.checkDCE != "function"))
    try {
      __REACT_DEVTOOLS_GLOBAL_HOOK__.checkDCE(Ro);
    } catch (l) {
      console.error(l);
    }
}
Ro(), Co.exports = zl;
var Bo = Co.exports;
/**
 * @license React
 * react-dom-client.production.js
 *
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */
var ol = Ym, Yo = A, ah = Bo;
function b(l) {
  var t = "https://react.dev/errors/" + l;
  if (1 < arguments.length) {
    t += "?args[]=" + encodeURIComponent(arguments[1]);
    for (var e = 2; e < arguments.length; e++)
      t += "&args[]=" + encodeURIComponent(arguments[e]);
  }
  return "Minified React error #" + l + "; visit " + t + " for the full message or use the non-minified dev environment for full errors and additional helpful warnings.";
}
function qo(l) {
  return !(!l || l.nodeType !== 1 && l.nodeType !== 9 && l.nodeType !== 11);
}
function tu(l) {
  var t = l, e = l;
  if (l.alternate) for (; t.return; ) t = t.return;
  else {
    l = t;
    do
      t = l, t.flags & 4098 && (e = t.return), l = t.return;
    while (l);
  }
  return t.tag === 3 ? e : null;
}
function Go(l) {
  if (l.tag === 13) {
    var t = l.memoizedState;
    if (t === null && (l = l.alternate, l !== null && (t = l.memoizedState)), t !== null) return t.dehydrated;
  }
  return null;
}
function Qo(l) {
  if (l.tag === 31) {
    var t = l.memoizedState;
    if (t === null && (l = l.alternate, l !== null && (t = l.memoizedState)), t !== null) return t.dehydrated;
  }
  return null;
}
function Lf(l) {
  if (tu(l) !== l)
    throw Error(b(188));
}
function uh(l) {
  var t = l.alternate;
  if (!t) {
    if (t = tu(l), t === null) throw Error(b(188));
    return t !== l ? null : l;
  }
  for (var e = l, a = t; ; ) {
    var u = e.return;
    if (u === null) break;
    var n = u.alternate;
    if (n === null) {
      if (a = u.return, a !== null) {
        e = a;
        continue;
      }
      break;
    }
    if (u.child === n.child) {
      for (n = u.child; n; ) {
        if (n === e) return Lf(u), l;
        if (n === a) return Lf(u), t;
        n = n.sibling;
      }
      throw Error(b(188));
    }
    if (e.return !== a.return) e = u, a = n;
    else {
      for (var i = !1, c = u.child; c; ) {
        if (c === e) {
          i = !0, e = u, a = n;
          break;
        }
        if (c === a) {
          i = !0, a = u, e = n;
          break;
        }
        c = c.sibling;
      }
      if (!i) {
        for (c = n.child; c; ) {
          if (c === e) {
            i = !0, e = n, a = u;
            break;
          }
          if (c === a) {
            i = !0, a = n, e = u;
            break;
          }
          c = c.sibling;
        }
        if (!i) throw Error(b(189));
      }
    }
    if (e.alternate !== a) throw Error(b(190));
  }
  if (e.tag !== 3) throw Error(b(188));
  return e.stateNode.current === e ? l : t;
}
function Xo(l) {
  var t = l.tag;
  if (t === 5 || t === 26 || t === 27 || t === 6) return l;
  for (l = l.child; l !== null; ) {
    if (t = Xo(l), t !== null) return t;
    l = l.sibling;
  }
  return null;
}
var P = Object.assign, nh = Symbol.for("react.element"), bu = Symbol.for("react.transitional.element"), pa = Symbol.for("react.portal"), Oe = Symbol.for("react.fragment"), Lo = Symbol.for("react.strict_mode"), Ci = Symbol.for("react.profiler"), Zo = Symbol.for("react.consumer"), ht = Symbol.for("react.context"), Rc = Symbol.for("react.forward_ref"), Hi = Symbol.for("react.suspense"), Ri = Symbol.for("react.suspense_list"), Bc = Symbol.for("react.memo"), Ot = Symbol.for("react.lazy"), Bi = Symbol.for("react.activity"), ih = Symbol.for("react.memo_cache_sentinel"), Zf = Symbol.iterator;
function ha(l) {
  return l === null || typeof l != "object" ? null : (l = Zf && l[Zf] || l["@@iterator"], typeof l == "function" ? l : null);
}
var ch = Symbol.for("react.client.reference");
function Yi(l) {
  if (l == null) return null;
  if (typeof l == "function")
    return l.$$typeof === ch ? null : l.displayName || l.name || null;
  if (typeof l == "string") return l;
  switch (l) {
    case Oe:
      return "Fragment";
    case Ci:
      return "Profiler";
    case Lo:
      return "StrictMode";
    case Hi:
      return "Suspense";
    case Ri:
      return "SuspenseList";
    case Bi:
      return "Activity";
  }
  if (typeof l == "object")
    switch (l.$$typeof) {
      case pa:
        return "Portal";
      case ht:
        return l.displayName || "Context";
      case Zo:
        return (l._context.displayName || "Context") + ".Consumer";
      case Rc:
        var t = l.render;
        return l = l.displayName, l || (l = t.displayName || t.name || "", l = l !== "" ? "ForwardRef(" + l + ")" : "ForwardRef"), l;
      case Bc:
        return t = l.displayName || null, t !== null ? t : Yi(l.type) || "Memo";
      case Ot:
        t = l._payload, l = l._init;
        try {
          return Yi(l(t));
        } catch {
        }
    }
  return null;
}
var Ta = Array.isArray, C = Yo.__CLIENT_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE, Z = ah.__DOM_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE, ne = {
  pending: !1,
  data: null,
  method: null,
  action: null
}, qi = [], De = -1;
function nt(l) {
  return { current: l };
}
function yl(l) {
  0 > De || (l.current = qi[De], qi[De] = null, De--);
}
function $(l, t) {
  De++, qi[De] = l.current, l.current = t;
}
var ut = nt(null), Xa = nt(null), qt = nt(null), $u = nt(null);
function Wu(l, t) {
  switch ($(qt, t), $(Xa, l), $(ut, null), t.nodeType) {
    case 9:
    case 11:
      l = (l = t.documentElement) && (l = l.namespaceURI) ? $s(l) : 0;
      break;
    default:
      if (l = t.tagName, t = t.namespaceURI)
        t = $s(t), l = sm(t, l);
      else
        switch (l) {
          case "svg":
            l = 1;
            break;
          case "math":
            l = 2;
            break;
          default:
            l = 0;
        }
  }
  yl(ut), $(ut, l);
}
function Ke() {
  yl(ut), yl(Xa), yl(qt);
}
function Gi(l) {
  l.memoizedState !== null && $($u, l);
  var t = ut.current, e = sm(t, l.type);
  t !== e && ($(Xa, l), $(ut, e));
}
function Fu(l) {
  Xa.current === l && (yl(ut), yl(Xa)), $u.current === l && (yl($u), Ia._currentValue = ne);
}
var $n, wf;
function te(l) {
  if ($n === void 0)
    try {
      throw Error();
    } catch (e) {
      var t = e.stack.trim().match(/\n( *(at )?)/);
      $n = t && t[1] || "", wf = -1 < e.stack.indexOf(`
    at`) ? " (<anonymous>)" : -1 < e.stack.indexOf("@") ? "@unknown:0:0" : "";
    }
  return `
` + $n + l + wf;
}
var Wn = !1;
function Fn(l, t) {
  if (!l || Wn) return "";
  Wn = !0;
  var e = Error.prepareStackTrace;
  Error.prepareStackTrace = void 0;
  try {
    var a = {
      DetermineComponentFrameRoot: function() {
        try {
          if (t) {
            var y = function() {
              throw Error();
            };
            if (Object.defineProperty(y.prototype, "props", {
              set: function() {
                throw Error();
              }
            }), typeof Reflect == "object" && Reflect.construct) {
              try {
                Reflect.construct(y, []);
              } catch (v) {
                var h = v;
              }
              Reflect.construct(l, [], y);
            } else {
              try {
                y.call();
              } catch (v) {
                h = v;
              }
              l.call(y.prototype);
            }
          } else {
            try {
              throw Error();
            } catch (v) {
              h = v;
            }
            (y = l()) && typeof y.catch == "function" && y.catch(function() {
            });
          }
        } catch (v) {
          if (v && h && typeof v.stack == "string")
            return [v.stack, h.stack];
        }
        return [null, null];
      }
    };
    a.DetermineComponentFrameRoot.displayName = "DetermineComponentFrameRoot";
    var u = Object.getOwnPropertyDescriptor(
      a.DetermineComponentFrameRoot,
      "name"
    );
    u && u.configurable && Object.defineProperty(
      a.DetermineComponentFrameRoot,
      "name",
      { value: "DetermineComponentFrameRoot" }
    );
    var n = a.DetermineComponentFrameRoot(), i = n[0], c = n[1];
    if (i && c) {
      var f = i.split(`
`), o = c.split(`
`);
      for (u = a = 0; a < f.length && !f[a].includes("DetermineComponentFrameRoot"); )
        a++;
      for (; u < o.length && !o[u].includes(
        "DetermineComponentFrameRoot"
      ); )
        u++;
      if (a === f.length || u === o.length)
        for (a = f.length - 1, u = o.length - 1; 1 <= a && 0 <= u && f[a] !== o[u]; )
          u--;
      for (; 1 <= a && 0 <= u; a--, u--)
        if (f[a] !== o[u]) {
          if (a !== 1 || u !== 1)
            do
              if (a--, u--, 0 > u || f[a] !== o[u]) {
                var d = `
` + f[a].replace(" at new ", " at ");
                return l.displayName && d.includes("<anonymous>") && (d = d.replace("<anonymous>", l.displayName)), d;
              }
            while (1 <= a && 0 <= u);
          break;
        }
    }
  } finally {
    Wn = !1, Error.prepareStackTrace = e;
  }
  return (e = l ? l.displayName || l.name : "") ? te(e) : "";
}
function fh(l, t) {
  switch (l.tag) {
    case 26:
    case 27:
    case 5:
      return te(l.type);
    case 16:
      return te("Lazy");
    case 13:
      return l.child !== t && t !== null ? te("Suspense Fallback") : te("Suspense");
    case 19:
      return te("SuspenseList");
    case 0:
    case 15:
      return Fn(l.type, !1);
    case 11:
      return Fn(l.type.render, !1);
    case 1:
      return Fn(l.type, !0);
    case 31:
      return te("Activity");
    default:
      return "";
  }
}
function Vf(l) {
  try {
    var t = "", e = null;
    do
      t += fh(l, e), e = l, l = l.return;
    while (l);
    return t;
  } catch (a) {
    return `
Error generating stack: ` + a.message + `
` + a.stack;
  }
}
var Qi = Object.prototype.hasOwnProperty, Yc = ol.unstable_scheduleCallback, In = ol.unstable_cancelCallback, sh = ol.unstable_shouldYield, oh = ol.unstable_requestPaint, Bl = ol.unstable_now, rh = ol.unstable_getCurrentPriorityLevel, wo = ol.unstable_ImmediatePriority, Vo = ol.unstable_UserBlockingPriority, Iu = ol.unstable_NormalPriority, dh = ol.unstable_LowPriority, Ko = ol.unstable_IdlePriority, mh = ol.log, hh = ol.unstable_setDisableYieldValue, eu = null, Yl = null;
function Ct(l) {
  if (typeof mh == "function" && hh(l), Yl && typeof Yl.setStrictMode == "function")
    try {
      Yl.setStrictMode(eu, l);
    } catch {
    }
}
var ql = Math.clz32 ? Math.clz32 : gh, yh = Math.log, vh = Math.LN2;
function gh(l) {
  return l >>>= 0, l === 0 ? 32 : 31 - (yh(l) / vh | 0) | 0;
}
var _u = 256, Eu = 262144, pu = 4194304;
function ee(l) {
  var t = l & 42;
  if (t !== 0) return t;
  switch (l & -l) {
    case 1:
      return 1;
    case 2:
      return 2;
    case 4:
      return 4;
    case 8:
      return 8;
    case 16:
      return 16;
    case 32:
      return 32;
    case 64:
      return 64;
    case 128:
      return 128;
    case 256:
    case 512:
    case 1024:
    case 2048:
    case 4096:
    case 8192:
    case 16384:
    case 32768:
    case 65536:
    case 131072:
      return l & 261888;
    case 262144:
    case 524288:
    case 1048576:
    case 2097152:
      return l & 3932160;
    case 4194304:
    case 8388608:
    case 16777216:
    case 33554432:
      return l & 62914560;
    case 67108864:
      return 67108864;
    case 134217728:
      return 134217728;
    case 268435456:
      return 268435456;
    case 536870912:
      return 536870912;
    case 1073741824:
      return 0;
    default:
      return l;
  }
}
function xn(l, t, e) {
  var a = l.pendingLanes;
  if (a === 0) return 0;
  var u = 0, n = l.suspendedLanes, i = l.pingedLanes;
  l = l.warmLanes;
  var c = a & 134217727;
  return c !== 0 ? (a = c & ~n, a !== 0 ? u = ee(a) : (i &= c, i !== 0 ? u = ee(i) : e || (e = c & ~l, e !== 0 && (u = ee(e))))) : (c = a & ~n, c !== 0 ? u = ee(c) : i !== 0 ? u = ee(i) : e || (e = a & ~l, e !== 0 && (u = ee(e)))), u === 0 ? 0 : t !== 0 && t !== u && !(t & n) && (n = u & -u, e = t & -t, n >= e || n === 32 && (e & 4194048) !== 0) ? t : u;
}
function au(l, t) {
  return (l.pendingLanes & ~(l.suspendedLanes & ~l.pingedLanes) & t) === 0;
}
function Sh(l, t) {
  switch (l) {
    case 1:
    case 2:
    case 4:
    case 8:
    case 64:
      return t + 250;
    case 16:
    case 32:
    case 128:
    case 256:
    case 512:
    case 1024:
    case 2048:
    case 4096:
    case 8192:
    case 16384:
    case 32768:
    case 65536:
    case 131072:
    case 262144:
    case 524288:
    case 1048576:
    case 2097152:
      return t + 5e3;
    case 4194304:
    case 8388608:
    case 16777216:
    case 33554432:
      return -1;
    case 67108864:
    case 134217728:
    case 268435456:
    case 536870912:
    case 1073741824:
      return -1;
    default:
      return -1;
  }
}
function Jo() {
  var l = pu;
  return pu <<= 1, !(pu & 62914560) && (pu = 4194304), l;
}
function Pn(l) {
  for (var t = [], e = 0; 31 > e; e++) t.push(l);
  return t;
}
function uu(l, t) {
  l.pendingLanes |= t, t !== 268435456 && (l.suspendedLanes = 0, l.pingedLanes = 0, l.warmLanes = 0);
}
function bh(l, t, e, a, u, n) {
  var i = l.pendingLanes;
  l.pendingLanes = e, l.suspendedLanes = 0, l.pingedLanes = 0, l.warmLanes = 0, l.expiredLanes &= e, l.entangledLanes &= e, l.errorRecoveryDisabledLanes &= e, l.shellSuspendCounter = 0;
  var c = l.entanglements, f = l.expirationTimes, o = l.hiddenUpdates;
  for (e = i & ~e; 0 < e; ) {
    var d = 31 - ql(e), y = 1 << d;
    c[d] = 0, f[d] = -1;
    var h = o[d];
    if (h !== null)
      for (o[d] = null, d = 0; d < h.length; d++) {
        var v = h[d];
        v !== null && (v.lane &= -536870913);
      }
    e &= ~y;
  }
  a !== 0 && ko(l, a, 0), n !== 0 && u === 0 && l.tag !== 0 && (l.suspendedLanes |= n & ~(i & ~t));
}
function ko(l, t, e) {
  l.pendingLanes |= t, l.suspendedLanes &= ~t;
  var a = 31 - ql(t);
  l.entangledLanes |= t, l.entanglements[a] = l.entanglements[a] | 1073741824 | e & 261930;
}
function $o(l, t) {
  var e = l.entangledLanes |= t;
  for (l = l.entanglements; e; ) {
    var a = 31 - ql(e), u = 1 << a;
    u & t | l[a] & t && (l[a] |= t), e &= ~u;
  }
}
function Wo(l, t) {
  var e = t & -t;
  return e = e & 42 ? 1 : qc(e), e & (l.suspendedLanes | t) ? 0 : e;
}
function qc(l) {
  switch (l) {
    case 2:
      l = 1;
      break;
    case 8:
      l = 4;
      break;
    case 32:
      l = 16;
      break;
    case 256:
    case 512:
    case 1024:
    case 2048:
    case 4096:
    case 8192:
    case 16384:
    case 32768:
    case 65536:
    case 131072:
    case 262144:
    case 524288:
    case 1048576:
    case 2097152:
    case 4194304:
    case 8388608:
    case 16777216:
    case 33554432:
      l = 128;
      break;
    case 268435456:
      l = 134217728;
      break;
    default:
      l = 0;
  }
  return l;
}
function Gc(l) {
  return l &= -l, 2 < l ? 8 < l ? l & 134217727 ? 32 : 268435456 : 8 : 2;
}
function Fo() {
  var l = Z.p;
  return l !== 0 ? l : (l = window.event, l === void 0 ? 32 : _m(l.type));
}
function Kf(l, t) {
  var e = Z.p;
  try {
    return Z.p = l, t();
  } finally {
    Z.p = e;
  }
}
var Ft = Math.random().toString(36).slice(2), gl = "__reactFiber$" + Ft, Nl = "__reactProps$" + Ft, na = "__reactContainer$" + Ft, Xi = "__reactEvents$" + Ft, _h = "__reactListeners$" + Ft, Eh = "__reactHandles$" + Ft, Jf = "__reactResources$" + Ft, nu = "__reactMarker$" + Ft;
function Qc(l) {
  delete l[gl], delete l[Nl], delete l[Xi], delete l[_h], delete l[Eh];
}
function xe(l) {
  var t = l[gl];
  if (t) return t;
  for (var e = l.parentNode; e; ) {
    if (t = e[na] || e[gl]) {
      if (e = t.alternate, t.child !== null || e !== null && e.child !== null)
        for (l = lo(l); l !== null; ) {
          if (e = l[gl]) return e;
          l = lo(l);
        }
      return t;
    }
    l = e, e = l.parentNode;
  }
  return null;
}
function ia(l) {
  if (l = l[gl] || l[na]) {
    var t = l.tag;
    if (t === 5 || t === 6 || t === 13 || t === 31 || t === 26 || t === 27 || t === 3)
      return l;
  }
  return null;
}
function za(l) {
  var t = l.tag;
  if (t === 5 || t === 26 || t === 27 || t === 6) return l.stateNode;
  throw Error(b(33));
}
function Ge(l) {
  var t = l[Jf];
  return t || (t = l[Jf] = { hoistableStyles: /* @__PURE__ */ new Map(), hoistableScripts: /* @__PURE__ */ new Map() }), t;
}
function hl(l) {
  l[nu] = !0;
}
var Io = /* @__PURE__ */ new Set(), Po = {};
function ye(l, t) {
  Je(l, t), Je(l + "Capture", t);
}
function Je(l, t) {
  for (Po[l] = t, l = 0; l < t.length; l++)
    Io.add(t[l]);
}
var ph = RegExp(
  "^[:A-Z_a-z\\u00C0-\\u00D6\\u00D8-\\u00F6\\u00F8-\\u02FF\\u0370-\\u037D\\u037F-\\u1FFF\\u200C-\\u200D\\u2070-\\u218F\\u2C00-\\u2FEF\\u3001-\\uD7FF\\uF900-\\uFDCF\\uFDF0-\\uFFFD][:A-Z_a-z\\u00C0-\\u00D6\\u00D8-\\u00F6\\u00F8-\\u02FF\\u0370-\\u037D\\u037F-\\u1FFF\\u200C-\\u200D\\u2070-\\u218F\\u2C00-\\u2FEF\\u3001-\\uD7FF\\uF900-\\uFDCF\\uFDF0-\\uFFFD\\-.0-9\\u00B7\\u0300-\\u036F\\u203F-\\u2040]*$"
), kf = {}, $f = {};
function Th(l) {
  return Qi.call($f, l) ? !0 : Qi.call(kf, l) ? !1 : ph.test(l) ? $f[l] = !0 : (kf[l] = !0, !1);
}
function Hu(l, t, e) {
  if (Th(t))
    if (e === null) l.removeAttribute(t);
    else {
      switch (typeof e) {
        case "undefined":
        case "function":
        case "symbol":
          l.removeAttribute(t);
          return;
        case "boolean":
          var a = t.toLowerCase().slice(0, 5);
          if (a !== "data-" && a !== "aria-") {
            l.removeAttribute(t);
            return;
          }
      }
      l.setAttribute(t, "" + e);
    }
}
function Tu(l, t, e) {
  if (e === null) l.removeAttribute(t);
  else {
    switch (typeof e) {
      case "undefined":
      case "function":
      case "symbol":
      case "boolean":
        l.removeAttribute(t);
        return;
    }
    l.setAttribute(t, "" + e);
  }
}
function ct(l, t, e, a) {
  if (a === null) l.removeAttribute(e);
  else {
    switch (typeof a) {
      case "undefined":
      case "function":
      case "symbol":
      case "boolean":
        l.removeAttribute(e);
        return;
    }
    l.setAttributeNS(t, e, "" + a);
  }
}
function Zl(l) {
  switch (typeof l) {
    case "bigint":
    case "boolean":
    case "number":
    case "string":
    case "undefined":
      return l;
    case "object":
      return l;
    default:
      return "";
  }
}
function lr(l) {
  var t = l.type;
  return (l = l.nodeName) && l.toLowerCase() === "input" && (t === "checkbox" || t === "radio");
}
function zh(l, t, e) {
  var a = Object.getOwnPropertyDescriptor(
    l.constructor.prototype,
    t
  );
  if (!l.hasOwnProperty(t) && typeof a < "u" && typeof a.get == "function" && typeof a.set == "function") {
    var u = a.get, n = a.set;
    return Object.defineProperty(l, t, {
      configurable: !0,
      get: function() {
        return u.call(this);
      },
      set: function(i) {
        e = "" + i, n.call(this, i);
      }
    }), Object.defineProperty(l, t, {
      enumerable: a.enumerable
    }), {
      getValue: function() {
        return e;
      },
      setValue: function(i) {
        e = "" + i;
      },
      stopTracking: function() {
        l._valueTracker = null, delete l[t];
      }
    };
  }
}
function Li(l) {
  if (!l._valueTracker) {
    var t = lr(l) ? "checked" : "value";
    l._valueTracker = zh(
      l,
      t,
      "" + l[t]
    );
  }
}
function tr(l) {
  if (!l) return !1;
  var t = l._valueTracker;
  if (!t) return !0;
  var e = t.getValue(), a = "";
  return l && (a = lr(l) ? l.checked ? "true" : "false" : l.value), l = a, l !== e ? (t.setValue(l), !0) : !1;
}
function Pu(l) {
  if (l = l || (typeof document < "u" ? document : void 0), typeof l > "u") return null;
  try {
    return l.activeElement || l.body;
  } catch {
    return l.body;
  }
}
var Ah = /[\n"\\]/g;
function Kl(l) {
  return l.replace(
    Ah,
    function(t) {
      return "\\" + t.charCodeAt(0).toString(16) + " ";
    }
  );
}
function Zi(l, t, e, a, u, n, i, c) {
  l.name = "", i != null && typeof i != "function" && typeof i != "symbol" && typeof i != "boolean" ? l.type = i : l.removeAttribute("type"), t != null ? i === "number" ? (t === 0 && l.value === "" || l.value != t) && (l.value = "" + Zl(t)) : l.value !== "" + Zl(t) && (l.value = "" + Zl(t)) : i !== "submit" && i !== "reset" || l.removeAttribute("value"), t != null ? wi(l, i, Zl(t)) : e != null ? wi(l, i, Zl(e)) : a != null && l.removeAttribute("value"), u == null && n != null && (l.defaultChecked = !!n), u != null && (l.checked = u && typeof u != "function" && typeof u != "symbol"), c != null && typeof c != "function" && typeof c != "symbol" && typeof c != "boolean" ? l.name = "" + Zl(c) : l.removeAttribute("name");
}
function er(l, t, e, a, u, n, i, c) {
  if (n != null && typeof n != "function" && typeof n != "symbol" && typeof n != "boolean" && (l.type = n), t != null || e != null) {
    if (!(n !== "submit" && n !== "reset" || t != null)) {
      Li(l);
      return;
    }
    e = e != null ? "" + Zl(e) : "", t = t != null ? "" + Zl(t) : e, c || t === l.value || (l.value = t), l.defaultValue = t;
  }
  a = a ?? u, a = typeof a != "function" && typeof a != "symbol" && !!a, l.checked = c ? l.checked : !!a, l.defaultChecked = !!a, i != null && typeof i != "function" && typeof i != "symbol" && typeof i != "boolean" && (l.name = i), Li(l);
}
function wi(l, t, e) {
  t === "number" && Pu(l.ownerDocument) === l || l.defaultValue === "" + e || (l.defaultValue = "" + e);
}
function Qe(l, t, e, a) {
  if (l = l.options, t) {
    t = {};
    for (var u = 0; u < e.length; u++)
      t["$" + e[u]] = !0;
    for (e = 0; e < l.length; e++)
      u = t.hasOwnProperty("$" + l[e].value), l[e].selected !== u && (l[e].selected = u), u && a && (l[e].defaultSelected = !0);
  } else {
    for (e = "" + Zl(e), t = null, u = 0; u < l.length; u++) {
      if (l[u].value === e) {
        l[u].selected = !0, a && (l[u].defaultSelected = !0);
        return;
      }
      t !== null || l[u].disabled || (t = l[u]);
    }
    t !== null && (t.selected = !0);
  }
}
function ar(l, t, e) {
  if (t != null && (t = "" + Zl(t), t !== l.value && (l.value = t), e == null)) {
    l.defaultValue !== t && (l.defaultValue = t);
    return;
  }
  l.defaultValue = e != null ? "" + Zl(e) : "";
}
function ur(l, t, e, a) {
  if (t == null) {
    if (a != null) {
      if (e != null) throw Error(b(92));
      if (Ta(a)) {
        if (1 < a.length) throw Error(b(93));
        a = a[0];
      }
      e = a;
    }
    e == null && (e = ""), t = e;
  }
  e = Zl(t), l.defaultValue = e, a = l.textContent, a === e && a !== "" && a !== null && (l.value = a), Li(l);
}
function ke(l, t) {
  if (t) {
    var e = l.firstChild;
    if (e && e === l.lastChild && e.nodeType === 3) {
      e.nodeValue = t;
      return;
    }
  }
  l.textContent = t;
}
var Mh = new Set(
  "animationIterationCount aspectRatio borderImageOutset borderImageSlice borderImageWidth boxFlex boxFlexGroup boxOrdinalGroup columnCount columns flex flexGrow flexPositive flexShrink flexNegative flexOrder gridArea gridRow gridRowEnd gridRowSpan gridRowStart gridColumn gridColumnEnd gridColumnSpan gridColumnStart fontWeight lineClamp lineHeight opacity order orphans scale tabSize widows zIndex zoom fillOpacity floodOpacity stopOpacity strokeDasharray strokeDashoffset strokeMiterlimit strokeOpacity strokeWidth MozAnimationIterationCount MozBoxFlex MozBoxFlexGroup MozLineClamp msAnimationIterationCount msFlex msZoom msFlexGrow msFlexNegative msFlexOrder msFlexPositive msFlexShrink msGridColumn msGridColumnSpan msGridRow msGridRowSpan WebkitAnimationIterationCount WebkitBoxFlex WebKitBoxFlexGroup WebkitBoxOrdinalGroup WebkitColumnCount WebkitColumns WebkitFlex WebkitFlexGrow WebkitFlexPositive WebkitFlexShrink WebkitLineClamp".split(
    " "
  )
);
function Wf(l, t, e) {
  var a = t.indexOf("--") === 0;
  e == null || typeof e == "boolean" || e === "" ? a ? l.setProperty(t, "") : t === "float" ? l.cssFloat = "" : l[t] = "" : a ? l.setProperty(t, e) : typeof e != "number" || e === 0 || Mh.has(t) ? t === "float" ? l.cssFloat = e : l[t] = ("" + e).trim() : l[t] = e + "px";
}
function nr(l, t, e) {
  if (t != null && typeof t != "object")
    throw Error(b(62));
  if (l = l.style, e != null) {
    for (var a in e)
      !e.hasOwnProperty(a) || t != null && t.hasOwnProperty(a) || (a.indexOf("--") === 0 ? l.setProperty(a, "") : a === "float" ? l.cssFloat = "" : l[a] = "");
    for (var u in t)
      a = t[u], t.hasOwnProperty(u) && e[u] !== a && Wf(l, u, a);
  } else
    for (var n in t)
      t.hasOwnProperty(n) && Wf(l, n, t[n]);
}
function Xc(l) {
  if (l.indexOf("-") === -1) return !1;
  switch (l) {
    case "annotation-xml":
    case "color-profile":
    case "font-face":
    case "font-face-src":
    case "font-face-uri":
    case "font-face-format":
    case "font-face-name":
    case "missing-glyph":
      return !1;
    default:
      return !0;
  }
}
var Oh = /* @__PURE__ */ new Map([
  ["acceptCharset", "accept-charset"],
  ["htmlFor", "for"],
  ["httpEquiv", "http-equiv"],
  ["crossOrigin", "crossorigin"],
  ["accentHeight", "accent-height"],
  ["alignmentBaseline", "alignment-baseline"],
  ["arabicForm", "arabic-form"],
  ["baselineShift", "baseline-shift"],
  ["capHeight", "cap-height"],
  ["clipPath", "clip-path"],
  ["clipRule", "clip-rule"],
  ["colorInterpolation", "color-interpolation"],
  ["colorInterpolationFilters", "color-interpolation-filters"],
  ["colorProfile", "color-profile"],
  ["colorRendering", "color-rendering"],
  ["dominantBaseline", "dominant-baseline"],
  ["enableBackground", "enable-background"],
  ["fillOpacity", "fill-opacity"],
  ["fillRule", "fill-rule"],
  ["floodColor", "flood-color"],
  ["floodOpacity", "flood-opacity"],
  ["fontFamily", "font-family"],
  ["fontSize", "font-size"],
  ["fontSizeAdjust", "font-size-adjust"],
  ["fontStretch", "font-stretch"],
  ["fontStyle", "font-style"],
  ["fontVariant", "font-variant"],
  ["fontWeight", "font-weight"],
  ["glyphName", "glyph-name"],
  ["glyphOrientationHorizontal", "glyph-orientation-horizontal"],
  ["glyphOrientationVertical", "glyph-orientation-vertical"],
  ["horizAdvX", "horiz-adv-x"],
  ["horizOriginX", "horiz-origin-x"],
  ["imageRendering", "image-rendering"],
  ["letterSpacing", "letter-spacing"],
  ["lightingColor", "lighting-color"],
  ["markerEnd", "marker-end"],
  ["markerMid", "marker-mid"],
  ["markerStart", "marker-start"],
  ["overlinePosition", "overline-position"],
  ["overlineThickness", "overline-thickness"],
  ["paintOrder", "paint-order"],
  ["panose-1", "panose-1"],
  ["pointerEvents", "pointer-events"],
  ["renderingIntent", "rendering-intent"],
  ["shapeRendering", "shape-rendering"],
  ["stopColor", "stop-color"],
  ["stopOpacity", "stop-opacity"],
  ["strikethroughPosition", "strikethrough-position"],
  ["strikethroughThickness", "strikethrough-thickness"],
  ["strokeDasharray", "stroke-dasharray"],
  ["strokeDashoffset", "stroke-dashoffset"],
  ["strokeLinecap", "stroke-linecap"],
  ["strokeLinejoin", "stroke-linejoin"],
  ["strokeMiterlimit", "stroke-miterlimit"],
  ["strokeOpacity", "stroke-opacity"],
  ["strokeWidth", "stroke-width"],
  ["textAnchor", "text-anchor"],
  ["textDecoration", "text-decoration"],
  ["textRendering", "text-rendering"],
  ["transformOrigin", "transform-origin"],
  ["underlinePosition", "underline-position"],
  ["underlineThickness", "underline-thickness"],
  ["unicodeBidi", "unicode-bidi"],
  ["unicodeRange", "unicode-range"],
  ["unitsPerEm", "units-per-em"],
  ["vAlphabetic", "v-alphabetic"],
  ["vHanging", "v-hanging"],
  ["vIdeographic", "v-ideographic"],
  ["vMathematical", "v-mathematical"],
  ["vectorEffect", "vector-effect"],
  ["vertAdvY", "vert-adv-y"],
  ["vertOriginX", "vert-origin-x"],
  ["vertOriginY", "vert-origin-y"],
  ["wordSpacing", "word-spacing"],
  ["writingMode", "writing-mode"],
  ["xmlnsXlink", "xmlns:xlink"],
  ["xHeight", "x-height"]
]), Dh = /^[\u0000-\u001F ]*j[\r\n\t]*a[\r\n\t]*v[\r\n\t]*a[\r\n\t]*s[\r\n\t]*c[\r\n\t]*r[\r\n\t]*i[\r\n\t]*p[\r\n\t]*t[\r\n\t]*:/i;
function Ru(l) {
  return Dh.test("" + l) ? "javascript:throw new Error('React has blocked a javascript: URL as a security precaution.')" : l;
}
function yt() {
}
var Vi = null;
function Lc(l) {
  return l = l.target || l.srcElement || window, l.correspondingUseElement && (l = l.correspondingUseElement), l.nodeType === 3 ? l.parentNode : l;
}
var Ne = null, Xe = null;
function Ff(l) {
  var t = ia(l);
  if (t && (l = t.stateNode)) {
    var e = l[Nl] || null;
    l: switch (l = t.stateNode, t.type) {
      case "input":
        if (Zi(
          l,
          e.value,
          e.defaultValue,
          e.defaultValue,
          e.checked,
          e.defaultChecked,
          e.type,
          e.name
        ), t = e.name, e.type === "radio" && t != null) {
          for (e = l; e.parentNode; ) e = e.parentNode;
          for (e = e.querySelectorAll(
            'input[name="' + Kl(
              "" + t
            ) + '"][type="radio"]'
          ), t = 0; t < e.length; t++) {
            var a = e[t];
            if (a !== l && a.form === l.form) {
              var u = a[Nl] || null;
              if (!u) throw Error(b(90));
              Zi(
                a,
                u.value,
                u.defaultValue,
                u.defaultValue,
                u.checked,
                u.defaultChecked,
                u.type,
                u.name
              );
            }
          }
          for (t = 0; t < e.length; t++)
            a = e[t], a.form === l.form && tr(a);
        }
        break l;
      case "textarea":
        ar(l, e.value, e.defaultValue);
        break l;
      case "select":
        t = e.value, t != null && Qe(l, !!e.multiple, t, !1);
    }
  }
}
var li = !1;
function ir(l, t, e) {
  if (li) return l(t, e);
  li = !0;
  try {
    var a = l(t);
    return a;
  } finally {
    if (li = !1, (Ne !== null || Xe !== null) && (Xn(), Ne && (t = Ne, l = Xe, Xe = Ne = null, Ff(t), l)))
      for (t = 0; t < l.length; t++) Ff(l[t]);
  }
}
function La(l, t) {
  var e = l.stateNode;
  if (e === null) return null;
  var a = e[Nl] || null;
  if (a === null) return null;
  e = a[t];
  l: switch (t) {
    case "onClick":
    case "onClickCapture":
    case "onDoubleClick":
    case "onDoubleClickCapture":
    case "onMouseDown":
    case "onMouseDownCapture":
    case "onMouseMove":
    case "onMouseMoveCapture":
    case "onMouseUp":
    case "onMouseUpCapture":
    case "onMouseEnter":
      (a = !a.disabled) || (l = l.type, a = !(l === "button" || l === "input" || l === "select" || l === "textarea")), l = !a;
      break l;
    default:
      l = !1;
  }
  if (l) return null;
  if (e && typeof e != "function")
    throw Error(
      b(231, t, typeof e)
    );
  return e;
}
var _t = !(typeof window > "u" || typeof window.document > "u" || typeof window.document.createElement > "u"), Ki = !1;
if (_t)
  try {
    var ya = {};
    Object.defineProperty(ya, "passive", {
      get: function() {
        Ki = !0;
      }
    }), window.addEventListener("test", ya, ya), window.removeEventListener("test", ya, ya);
  } catch {
    Ki = !1;
  }
var Ht = null, Zc = null, Bu = null;
function cr() {
  if (Bu) return Bu;
  var l, t = Zc, e = t.length, a, u = "value" in Ht ? Ht.value : Ht.textContent, n = u.length;
  for (l = 0; l < e && t[l] === u[l]; l++) ;
  var i = e - l;
  for (a = 1; a <= i && t[e - a] === u[n - a]; a++) ;
  return Bu = u.slice(l, 1 < a ? 1 - a : void 0);
}
function Yu(l) {
  var t = l.keyCode;
  return "charCode" in l ? (l = l.charCode, l === 0 && t === 13 && (l = 13)) : l = t, l === 10 && (l = 13), 32 <= l || l === 13 ? l : 0;
}
function zu() {
  return !0;
}
function If() {
  return !1;
}
function jl(l) {
  function t(e, a, u, n, i) {
    this._reactName = e, this._targetInst = u, this.type = a, this.nativeEvent = n, this.target = i, this.currentTarget = null;
    for (var c in l)
      l.hasOwnProperty(c) && (e = l[c], this[c] = e ? e(n) : n[c]);
    return this.isDefaultPrevented = (n.defaultPrevented != null ? n.defaultPrevented : n.returnValue === !1) ? zu : If, this.isPropagationStopped = If, this;
  }
  return P(t.prototype, {
    preventDefault: function() {
      this.defaultPrevented = !0;
      var e = this.nativeEvent;
      e && (e.preventDefault ? e.preventDefault() : typeof e.returnValue != "unknown" && (e.returnValue = !1), this.isDefaultPrevented = zu);
    },
    stopPropagation: function() {
      var e = this.nativeEvent;
      e && (e.stopPropagation ? e.stopPropagation() : typeof e.cancelBubble != "unknown" && (e.cancelBubble = !0), this.isPropagationStopped = zu);
    },
    persist: function() {
    },
    isPersistent: zu
  }), t;
}
var ve = {
  eventPhase: 0,
  bubbles: 0,
  cancelable: 0,
  timeStamp: function(l) {
    return l.timeStamp || Date.now();
  },
  defaultPrevented: 0,
  isTrusted: 0
}, Nn = jl(ve), iu = P({}, ve, { view: 0, detail: 0 }), xh = jl(iu), ti, ei, va, jn = P({}, iu, {
  screenX: 0,
  screenY: 0,
  clientX: 0,
  clientY: 0,
  pageX: 0,
  pageY: 0,
  ctrlKey: 0,
  shiftKey: 0,
  altKey: 0,
  metaKey: 0,
  getModifierState: wc,
  button: 0,
  buttons: 0,
  relatedTarget: function(l) {
    return l.relatedTarget === void 0 ? l.fromElement === l.srcElement ? l.toElement : l.fromElement : l.relatedTarget;
  },
  movementX: function(l) {
    return "movementX" in l ? l.movementX : (l !== va && (va && l.type === "mousemove" ? (ti = l.screenX - va.screenX, ei = l.screenY - va.screenY) : ei = ti = 0, va = l), ti);
  },
  movementY: function(l) {
    return "movementY" in l ? l.movementY : ei;
  }
}), Pf = jl(jn), Nh = P({}, jn, { dataTransfer: 0 }), jh = jl(Nh), Uh = P({}, iu, { relatedTarget: 0 }), ai = jl(Uh), Ch = P({}, ve, {
  animationName: 0,
  elapsedTime: 0,
  pseudoElement: 0
}), Hh = jl(Ch), Rh = P({}, ve, {
  clipboardData: function(l) {
    return "clipboardData" in l ? l.clipboardData : window.clipboardData;
  }
}), Bh = jl(Rh), Yh = P({}, ve, { data: 0 }), ls = jl(Yh), qh = {
  Esc: "Escape",
  Spacebar: " ",
  Left: "ArrowLeft",
  Up: "ArrowUp",
  Right: "ArrowRight",
  Down: "ArrowDown",
  Del: "Delete",
  Win: "OS",
  Menu: "ContextMenu",
  Apps: "ContextMenu",
  Scroll: "ScrollLock",
  MozPrintableKey: "Unidentified"
}, Gh = {
  8: "Backspace",
  9: "Tab",
  12: "Clear",
  13: "Enter",
  16: "Shift",
  17: "Control",
  18: "Alt",
  19: "Pause",
  20: "CapsLock",
  27: "Escape",
  32: " ",
  33: "PageUp",
  34: "PageDown",
  35: "End",
  36: "Home",
  37: "ArrowLeft",
  38: "ArrowUp",
  39: "ArrowRight",
  40: "ArrowDown",
  45: "Insert",
  46: "Delete",
  112: "F1",
  113: "F2",
  114: "F3",
  115: "F4",
  116: "F5",
  117: "F6",
  118: "F7",
  119: "F8",
  120: "F9",
  121: "F10",
  122: "F11",
  123: "F12",
  144: "NumLock",
  145: "ScrollLock",
  224: "Meta"
}, Qh = {
  Alt: "altKey",
  Control: "ctrlKey",
  Meta: "metaKey",
  Shift: "shiftKey"
};
function Xh(l) {
  var t = this.nativeEvent;
  return t.getModifierState ? t.getModifierState(l) : (l = Qh[l]) ? !!t[l] : !1;
}
function wc() {
  return Xh;
}
var Lh = P({}, iu, {
  key: function(l) {
    if (l.key) {
      var t = qh[l.key] || l.key;
      if (t !== "Unidentified") return t;
    }
    return l.type === "keypress" ? (l = Yu(l), l === 13 ? "Enter" : String.fromCharCode(l)) : l.type === "keydown" || l.type === "keyup" ? Gh[l.keyCode] || "Unidentified" : "";
  },
  code: 0,
  location: 0,
  ctrlKey: 0,
  shiftKey: 0,
  altKey: 0,
  metaKey: 0,
  repeat: 0,
  locale: 0,
  getModifierState: wc,
  charCode: function(l) {
    return l.type === "keypress" ? Yu(l) : 0;
  },
  keyCode: function(l) {
    return l.type === "keydown" || l.type === "keyup" ? l.keyCode : 0;
  },
  which: function(l) {
    return l.type === "keypress" ? Yu(l) : l.type === "keydown" || l.type === "keyup" ? l.keyCode : 0;
  }
}), Zh = jl(Lh), wh = P({}, jn, {
  pointerId: 0,
  width: 0,
  height: 0,
  pressure: 0,
  tangentialPressure: 0,
  tiltX: 0,
  tiltY: 0,
  twist: 0,
  pointerType: 0,
  isPrimary: 0
}), ts = jl(wh), Vh = P({}, iu, {
  touches: 0,
  targetTouches: 0,
  changedTouches: 0,
  altKey: 0,
  metaKey: 0,
  ctrlKey: 0,
  shiftKey: 0,
  getModifierState: wc
}), Kh = jl(Vh), Jh = P({}, ve, {
  propertyName: 0,
  elapsedTime: 0,
  pseudoElement: 0
}), kh = jl(Jh), $h = P({}, jn, {
  deltaX: function(l) {
    return "deltaX" in l ? l.deltaX : "wheelDeltaX" in l ? -l.wheelDeltaX : 0;
  },
  deltaY: function(l) {
    return "deltaY" in l ? l.deltaY : "wheelDeltaY" in l ? -l.wheelDeltaY : "wheelDelta" in l ? -l.wheelDelta : 0;
  },
  deltaZ: 0,
  deltaMode: 0
}), Wh = jl($h), Fh = P({}, ve, {
  newState: 0,
  oldState: 0
}), Ih = jl(Fh), Ph = [9, 13, 27, 32], Vc = _t && "CompositionEvent" in window, Da = null;
_t && "documentMode" in document && (Da = document.documentMode);
var l0 = _t && "TextEvent" in window && !Da, fr = _t && (!Vc || Da && 8 < Da && 11 >= Da), es = " ", as = !1;
function sr(l, t) {
  switch (l) {
    case "keyup":
      return Ph.indexOf(t.keyCode) !== -1;
    case "keydown":
      return t.keyCode !== 229;
    case "keypress":
    case "mousedown":
    case "focusout":
      return !0;
    default:
      return !1;
  }
}
function or(l) {
  return l = l.detail, typeof l == "object" && "data" in l ? l.data : null;
}
var je = !1;
function t0(l, t) {
  switch (l) {
    case "compositionend":
      return or(t);
    case "keypress":
      return t.which !== 32 ? null : (as = !0, es);
    case "textInput":
      return l = t.data, l === es && as ? null : l;
    default:
      return null;
  }
}
function e0(l, t) {
  if (je)
    return l === "compositionend" || !Vc && sr(l, t) ? (l = cr(), Bu = Zc = Ht = null, je = !1, l) : null;
  switch (l) {
    case "paste":
      return null;
    case "keypress":
      if (!(t.ctrlKey || t.altKey || t.metaKey) || t.ctrlKey && t.altKey) {
        if (t.char && 1 < t.char.length)
          return t.char;
        if (t.which) return String.fromCharCode(t.which);
      }
      return null;
    case "compositionend":
      return fr && t.locale !== "ko" ? null : t.data;
    default:
      return null;
  }
}
var a0 = {
  color: !0,
  date: !0,
  datetime: !0,
  "datetime-local": !0,
  email: !0,
  month: !0,
  number: !0,
  password: !0,
  range: !0,
  search: !0,
  tel: !0,
  text: !0,
  time: !0,
  url: !0,
  week: !0
};
function us(l) {
  var t = l && l.nodeName && l.nodeName.toLowerCase();
  return t === "input" ? !!a0[l.type] : t === "textarea";
}
function rr(l, t, e, a) {
  Ne ? Xe ? Xe.push(a) : Xe = [a] : Ne = a, t = Sn(t, "onChange"), 0 < t.length && (e = new Nn(
    "onChange",
    "change",
    null,
    e,
    a
  ), l.push({ event: e, listeners: t }));
}
var xa = null, Za = null;
function u0(l) {
  im(l, 0);
}
function Un(l) {
  var t = za(l);
  if (tr(t)) return l;
}
function ns(l, t) {
  if (l === "change") return t;
}
var dr = !1;
if (_t) {
  var ui;
  if (_t) {
    var ni = "oninput" in document;
    if (!ni) {
      var is = document.createElement("div");
      is.setAttribute("oninput", "return;"), ni = typeof is.oninput == "function";
    }
    ui = ni;
  } else ui = !1;
  dr = ui && (!document.documentMode || 9 < document.documentMode);
}
function cs() {
  xa && (xa.detachEvent("onpropertychange", mr), Za = xa = null);
}
function mr(l) {
  if (l.propertyName === "value" && Un(Za)) {
    var t = [];
    rr(
      t,
      Za,
      l,
      Lc(l)
    ), ir(u0, t);
  }
}
function n0(l, t, e) {
  l === "focusin" ? (cs(), xa = t, Za = e, xa.attachEvent("onpropertychange", mr)) : l === "focusout" && cs();
}
function i0(l) {
  if (l === "selectionchange" || l === "keyup" || l === "keydown")
    return Un(Za);
}
function c0(l, t) {
  if (l === "click") return Un(t);
}
function f0(l, t) {
  if (l === "input" || l === "change")
    return Un(t);
}
function s0(l, t) {
  return l === t && (l !== 0 || 1 / l === 1 / t) || l !== l && t !== t;
}
var Ql = typeof Object.is == "function" ? Object.is : s0;
function wa(l, t) {
  if (Ql(l, t)) return !0;
  if (typeof l != "object" || l === null || typeof t != "object" || t === null)
    return !1;
  var e = Object.keys(l), a = Object.keys(t);
  if (e.length !== a.length) return !1;
  for (a = 0; a < e.length; a++) {
    var u = e[a];
    if (!Qi.call(t, u) || !Ql(l[u], t[u]))
      return !1;
  }
  return !0;
}
function fs(l) {
  for (; l && l.firstChild; ) l = l.firstChild;
  return l;
}
function ss(l, t) {
  var e = fs(l);
  l = 0;
  for (var a; e; ) {
    if (e.nodeType === 3) {
      if (a = l + e.textContent.length, l <= t && a >= t)
        return { node: e, offset: t - l };
      l = a;
    }
    l: {
      for (; e; ) {
        if (e.nextSibling) {
          e = e.nextSibling;
          break l;
        }
        e = e.parentNode;
      }
      e = void 0;
    }
    e = fs(e);
  }
}
function hr(l, t) {
  return l && t ? l === t ? !0 : l && l.nodeType === 3 ? !1 : t && t.nodeType === 3 ? hr(l, t.parentNode) : "contains" in l ? l.contains(t) : l.compareDocumentPosition ? !!(l.compareDocumentPosition(t) & 16) : !1 : !1;
}
function yr(l) {
  l = l != null && l.ownerDocument != null && l.ownerDocument.defaultView != null ? l.ownerDocument.defaultView : window;
  for (var t = Pu(l.document); t instanceof l.HTMLIFrameElement; ) {
    try {
      var e = typeof t.contentWindow.location.href == "string";
    } catch {
      e = !1;
    }
    if (e) l = t.contentWindow;
    else break;
    t = Pu(l.document);
  }
  return t;
}
function Kc(l) {
  var t = l && l.nodeName && l.nodeName.toLowerCase();
  return t && (t === "input" && (l.type === "text" || l.type === "search" || l.type === "tel" || l.type === "url" || l.type === "password") || t === "textarea" || l.contentEditable === "true");
}
var o0 = _t && "documentMode" in document && 11 >= document.documentMode, Ue = null, Ji = null, Na = null, ki = !1;
function os(l, t, e) {
  var a = e.window === e ? e.document : e.nodeType === 9 ? e : e.ownerDocument;
  ki || Ue == null || Ue !== Pu(a) || (a = Ue, "selectionStart" in a && Kc(a) ? a = { start: a.selectionStart, end: a.selectionEnd } : (a = (a.ownerDocument && a.ownerDocument.defaultView || window).getSelection(), a = {
    anchorNode: a.anchorNode,
    anchorOffset: a.anchorOffset,
    focusNode: a.focusNode,
    focusOffset: a.focusOffset
  }), Na && wa(Na, a) || (Na = a, a = Sn(Ji, "onSelect"), 0 < a.length && (t = new Nn(
    "onSelect",
    "select",
    null,
    t,
    e
  ), l.push({ event: t, listeners: a }), t.target = Ue)));
}
function le(l, t) {
  var e = {};
  return e[l.toLowerCase()] = t.toLowerCase(), e["Webkit" + l] = "webkit" + t, e["Moz" + l] = "moz" + t, e;
}
var Ce = {
  animationend: le("Animation", "AnimationEnd"),
  animationiteration: le("Animation", "AnimationIteration"),
  animationstart: le("Animation", "AnimationStart"),
  transitionrun: le("Transition", "TransitionRun"),
  transitionstart: le("Transition", "TransitionStart"),
  transitioncancel: le("Transition", "TransitionCancel"),
  transitionend: le("Transition", "TransitionEnd")
}, ii = {}, vr = {};
_t && (vr = document.createElement("div").style, "AnimationEvent" in window || (delete Ce.animationend.animation, delete Ce.animationiteration.animation, delete Ce.animationstart.animation), "TransitionEvent" in window || delete Ce.transitionend.transition);
function ge(l) {
  if (ii[l]) return ii[l];
  if (!Ce[l]) return l;
  var t = Ce[l], e;
  for (e in t)
    if (t.hasOwnProperty(e) && e in vr)
      return ii[l] = t[e];
  return l;
}
var gr = ge("animationend"), Sr = ge("animationiteration"), br = ge("animationstart"), r0 = ge("transitionrun"), d0 = ge("transitionstart"), m0 = ge("transitioncancel"), _r = ge("transitionend"), Er = /* @__PURE__ */ new Map(), $i = "abort auxClick beforeToggle cancel canPlay canPlayThrough click close contextMenu copy cut drag dragEnd dragEnter dragExit dragLeave dragOver dragStart drop durationChange emptied encrypted ended error gotPointerCapture input invalid keyDown keyPress keyUp load loadedData loadedMetadata loadStart lostPointerCapture mouseDown mouseMove mouseOut mouseOver mouseUp paste pause play playing pointerCancel pointerDown pointerMove pointerOut pointerOver pointerUp progress rateChange reset resize seeked seeking stalled submit suspend timeUpdate touchCancel touchEnd touchStart volumeChange scroll toggle touchMove waiting wheel".split(
  " "
);
$i.push("scrollEnd");
function lt(l, t) {
  Er.set(l, t), ye(t, [l]);
}
var ln = typeof reportError == "function" ? reportError : function(l) {
  if (typeof window == "object" && typeof window.ErrorEvent == "function") {
    var t = new window.ErrorEvent("error", {
      bubbles: !0,
      cancelable: !0,
      message: typeof l == "object" && l !== null && typeof l.message == "string" ? String(l.message) : String(l),
      error: l
    });
    if (!window.dispatchEvent(t)) return;
  } else if (typeof process == "object" && typeof process.emit == "function") {
    process.emit("uncaughtException", l);
    return;
  }
  console.error(l);
}, Ll = [], He = 0, Jc = 0;
function Cn() {
  for (var l = He, t = Jc = He = 0; t < l; ) {
    var e = Ll[t];
    Ll[t++] = null;
    var a = Ll[t];
    Ll[t++] = null;
    var u = Ll[t];
    Ll[t++] = null;
    var n = Ll[t];
    if (Ll[t++] = null, a !== null && u !== null) {
      var i = a.pending;
      i === null ? u.next = u : (u.next = i.next, i.next = u), a.pending = u;
    }
    n !== 0 && pr(e, u, n);
  }
}
function Hn(l, t, e, a) {
  Ll[He++] = l, Ll[He++] = t, Ll[He++] = e, Ll[He++] = a, Jc |= a, l.lanes |= a, l = l.alternate, l !== null && (l.lanes |= a);
}
function kc(l, t, e, a) {
  return Hn(l, t, e, a), tn(l);
}
function Se(l, t) {
  return Hn(l, null, null, t), tn(l);
}
function pr(l, t, e) {
  l.lanes |= e;
  var a = l.alternate;
  a !== null && (a.lanes |= e);
  for (var u = !1, n = l.return; n !== null; )
    n.childLanes |= e, a = n.alternate, a !== null && (a.childLanes |= e), n.tag === 22 && (l = n.stateNode, l === null || l._visibility & 1 || (u = !0)), l = n, n = n.return;
  return l.tag === 3 ? (n = l.stateNode, u && t !== null && (u = 31 - ql(e), l = n.hiddenUpdates, a = l[u], a === null ? l[u] = [t] : a.push(t), t.lane = e | 536870912), n) : null;
}
function tn(l) {
  if (50 < Ga)
    throw Ga = 0, vc = null, Error(b(185));
  for (var t = l.return; t !== null; )
    l = t, t = l.return;
  return l.tag === 3 ? l.stateNode : null;
}
var Re = {};
function h0(l, t, e, a) {
  this.tag = l, this.key = e, this.sibling = this.child = this.return = this.stateNode = this.type = this.elementType = null, this.index = 0, this.refCleanup = this.ref = null, this.pendingProps = t, this.dependencies = this.memoizedState = this.updateQueue = this.memoizedProps = null, this.mode = a, this.subtreeFlags = this.flags = 0, this.deletions = null, this.childLanes = this.lanes = 0, this.alternate = null;
}
function Hl(l, t, e, a) {
  return new h0(l, t, e, a);
}
function $c(l) {
  return l = l.prototype, !(!l || !l.isReactComponent);
}
function gt(l, t) {
  var e = l.alternate;
  return e === null ? (e = Hl(
    l.tag,
    t,
    l.key,
    l.mode
  ), e.elementType = l.elementType, e.type = l.type, e.stateNode = l.stateNode, e.alternate = l, l.alternate = e) : (e.pendingProps = t, e.type = l.type, e.flags = 0, e.subtreeFlags = 0, e.deletions = null), e.flags = l.flags & 65011712, e.childLanes = l.childLanes, e.lanes = l.lanes, e.child = l.child, e.memoizedProps = l.memoizedProps, e.memoizedState = l.memoizedState, e.updateQueue = l.updateQueue, t = l.dependencies, e.dependencies = t === null ? null : { lanes: t.lanes, firstContext: t.firstContext }, e.sibling = l.sibling, e.index = l.index, e.ref = l.ref, e.refCleanup = l.refCleanup, e;
}
function Tr(l, t) {
  l.flags &= 65011714;
  var e = l.alternate;
  return e === null ? (l.childLanes = 0, l.lanes = t, l.child = null, l.subtreeFlags = 0, l.memoizedProps = null, l.memoizedState = null, l.updateQueue = null, l.dependencies = null, l.stateNode = null) : (l.childLanes = e.childLanes, l.lanes = e.lanes, l.child = e.child, l.subtreeFlags = 0, l.deletions = null, l.memoizedProps = e.memoizedProps, l.memoizedState = e.memoizedState, l.updateQueue = e.updateQueue, l.type = e.type, t = e.dependencies, l.dependencies = t === null ? null : {
    lanes: t.lanes,
    firstContext: t.firstContext
  }), l;
}
function qu(l, t, e, a, u, n) {
  var i = 0;
  if (a = l, typeof l == "function") $c(l) && (i = 1);
  else if (typeof l == "string")
    i = by(
      l,
      e,
      ut.current
    ) ? 26 : l === "html" || l === "head" || l === "body" ? 27 : 5;
  else
    l: switch (l) {
      case Bi:
        return l = Hl(31, e, t, u), l.elementType = Bi, l.lanes = n, l;
      case Oe:
        return ie(e.children, u, n, t);
      case Lo:
        i = 8, u |= 24;
        break;
      case Ci:
        return l = Hl(12, e, t, u | 2), l.elementType = Ci, l.lanes = n, l;
      case Hi:
        return l = Hl(13, e, t, u), l.elementType = Hi, l.lanes = n, l;
      case Ri:
        return l = Hl(19, e, t, u), l.elementType = Ri, l.lanes = n, l;
      default:
        if (typeof l == "object" && l !== null)
          switch (l.$$typeof) {
            case ht:
              i = 10;
              break l;
            case Zo:
              i = 9;
              break l;
            case Rc:
              i = 11;
              break l;
            case Bc:
              i = 14;
              break l;
            case Ot:
              i = 16, a = null;
              break l;
          }
        i = 29, e = Error(
          b(130, l === null ? "null" : typeof l, "")
        ), a = null;
    }
  return t = Hl(i, e, t, u), t.elementType = l, t.type = a, t.lanes = n, t;
}
function ie(l, t, e, a) {
  return l = Hl(7, l, a, t), l.lanes = e, l;
}
function ci(l, t, e) {
  return l = Hl(6, l, null, t), l.lanes = e, l;
}
function zr(l) {
  var t = Hl(18, null, null, 0);
  return t.stateNode = l, t;
}
function fi(l, t, e) {
  return t = Hl(
    4,
    l.children !== null ? l.children : [],
    l.key,
    t
  ), t.lanes = e, t.stateNode = {
    containerInfo: l.containerInfo,
    pendingChildren: null,
    implementation: l.implementation
  }, t;
}
var rs = /* @__PURE__ */ new WeakMap();
function Jl(l, t) {
  if (typeof l == "object" && l !== null) {
    var e = rs.get(l);
    return e !== void 0 ? e : (t = {
      value: l,
      source: t,
      stack: Vf(t)
    }, rs.set(l, t), t);
  }
  return {
    value: l,
    source: t,
    stack: Vf(t)
  };
}
var Be = [], Ye = 0, en = null, Va = 0, wl = [], Vl = 0, Jt = null, tt = 1, et = "";
function dt(l, t) {
  Be[Ye++] = Va, Be[Ye++] = en, en = l, Va = t;
}
function Ar(l, t, e) {
  wl[Vl++] = tt, wl[Vl++] = et, wl[Vl++] = Jt, Jt = l;
  var a = tt;
  l = et;
  var u = 32 - ql(a) - 1;
  a &= ~(1 << u), e += 1;
  var n = 32 - ql(t) + u;
  if (30 < n) {
    var i = u - u % 5;
    n = (a & (1 << i) - 1).toString(32), a >>= i, u -= i, tt = 1 << 32 - ql(t) + u | e << u | a, et = n + l;
  } else
    tt = 1 << n | e << u | a, et = l;
}
function Wc(l) {
  l.return !== null && (dt(l, 1), Ar(l, 1, 0));
}
function Fc(l) {
  for (; l === en; )
    en = Be[--Ye], Be[Ye] = null, Va = Be[--Ye], Be[Ye] = null;
  for (; l === Jt; )
    Jt = wl[--Vl], wl[Vl] = null, et = wl[--Vl], wl[Vl] = null, tt = wl[--Vl], wl[Vl] = null;
}
function Mr(l, t) {
  wl[Vl++] = tt, wl[Vl++] = et, wl[Vl++] = Jt, tt = t.id, et = t.overflow, Jt = l;
}
var Sl = null, F = null, Q = !1, Gt = null, kl = !1, Wi = Error(b(519));
function kt(l) {
  var t = Error(
    b(
      418,
      1 < arguments.length && arguments[1] !== void 0 && arguments[1] ? "text" : "HTML",
      ""
    )
  );
  throw Ka(Jl(t, l)), Wi;
}
function ds(l) {
  var t = l.stateNode, e = l.type, a = l.memoizedProps;
  switch (t[gl] = l, t[Nl] = a, e) {
    case "dialog":
      Y("cancel", t), Y("close", t);
      break;
    case "iframe":
    case "object":
    case "embed":
      Y("load", t);
      break;
    case "video":
    case "audio":
      for (e = 0; e < Wa.length; e++)
        Y(Wa[e], t);
      break;
    case "source":
      Y("error", t);
      break;
    case "img":
    case "image":
    case "link":
      Y("error", t), Y("load", t);
      break;
    case "details":
      Y("toggle", t);
      break;
    case "input":
      Y("invalid", t), er(
        t,
        a.value,
        a.defaultValue,
        a.checked,
        a.defaultChecked,
        a.type,
        a.name,
        !0
      );
      break;
    case "select":
      Y("invalid", t);
      break;
    case "textarea":
      Y("invalid", t), ur(t, a.value, a.defaultValue, a.children);
  }
  e = a.children, typeof e != "string" && typeof e != "number" && typeof e != "bigint" || t.textContent === "" + e || a.suppressHydrationWarning === !0 || fm(t.textContent, e) ? (a.popover != null && (Y("beforetoggle", t), Y("toggle", t)), a.onScroll != null && Y("scroll", t), a.onScrollEnd != null && Y("scrollend", t), a.onClick != null && (t.onclick = yt), t = !0) : t = !1, t || kt(l, !0);
}
function ms(l) {
  for (Sl = l.return; Sl; )
    switch (Sl.tag) {
      case 5:
      case 31:
      case 13:
        kl = !1;
        return;
      case 27:
      case 3:
        kl = !0;
        return;
      default:
        Sl = Sl.return;
    }
}
function Ee(l) {
  if (l !== Sl) return !1;
  if (!Q) return ms(l), Q = !0, !1;
  var t = l.tag, e;
  if ((e = t !== 3 && t !== 27) && ((e = t === 5) && (e = l.type, e = !(e !== "form" && e !== "button") || Ec(l.type, l.memoizedProps)), e = !e), e && F && kt(l), ms(l), t === 13) {
    if (l = l.memoizedState, l = l !== null ? l.dehydrated : null, !l) throw Error(b(317));
    F = Ps(l);
  } else if (t === 31) {
    if (l = l.memoizedState, l = l !== null ? l.dehydrated : null, !l) throw Error(b(317));
    F = Ps(l);
  } else
    t === 27 ? (t = F, It(l.type) ? (l = Ac, Ac = null, F = l) : F = t) : F = Sl ? Wl(l.stateNode.nextSibling) : null;
  return !0;
}
function oe() {
  F = Sl = null, Q = !1;
}
function si() {
  var l = Gt;
  return l !== null && (Dl === null ? Dl = l : Dl.push.apply(
    Dl,
    l
  ), Gt = null), l;
}
function Ka(l) {
  Gt === null ? Gt = [l] : Gt.push(l);
}
var Fi = nt(null), be = null, vt = null;
function xt(l, t, e) {
  $(Fi, t._currentValue), t._currentValue = e;
}
function St(l) {
  l._currentValue = Fi.current, yl(Fi);
}
function Ii(l, t, e) {
  for (; l !== null; ) {
    var a = l.alternate;
    if ((l.childLanes & t) !== t ? (l.childLanes |= t, a !== null && (a.childLanes |= t)) : a !== null && (a.childLanes & t) !== t && (a.childLanes |= t), l === e) break;
    l = l.return;
  }
}
function Pi(l, t, e, a) {
  var u = l.child;
  for (u !== null && (u.return = l); u !== null; ) {
    var n = u.dependencies;
    if (n !== null) {
      var i = u.child;
      n = n.firstContext;
      l: for (; n !== null; ) {
        var c = n;
        n = u;
        for (var f = 0; f < t.length; f++)
          if (c.context === t[f]) {
            n.lanes |= e, c = n.alternate, c !== null && (c.lanes |= e), Ii(
              n.return,
              e,
              l
            ), a || (i = null);
            break l;
          }
        n = c.next;
      }
    } else if (u.tag === 18) {
      if (i = u.return, i === null) throw Error(b(341));
      i.lanes |= e, n = i.alternate, n !== null && (n.lanes |= e), Ii(i, e, l), i = null;
    } else i = u.child;
    if (i !== null) i.return = u;
    else
      for (i = u; i !== null; ) {
        if (i === l) {
          i = null;
          break;
        }
        if (u = i.sibling, u !== null) {
          u.return = i.return, i = u;
          break;
        }
        i = i.return;
      }
    u = i;
  }
}
function ca(l, t, e, a) {
  l = null;
  for (var u = t, n = !1; u !== null; ) {
    if (!n) {
      if (u.flags & 524288) n = !0;
      else if (u.flags & 262144) break;
    }
    if (u.tag === 10) {
      var i = u.alternate;
      if (i === null) throw Error(b(387));
      if (i = i.memoizedProps, i !== null) {
        var c = u.type;
        Ql(u.pendingProps.value, i.value) || (l !== null ? l.push(c) : l = [c]);
      }
    } else if (u === $u.current) {
      if (i = u.alternate, i === null) throw Error(b(387));
      i.memoizedState.memoizedState !== u.memoizedState.memoizedState && (l !== null ? l.push(Ia) : l = [Ia]);
    }
    u = u.return;
  }
  l !== null && Pi(
    t,
    l,
    e,
    a
  ), t.flags |= 262144;
}
function an(l) {
  for (l = l.firstContext; l !== null; ) {
    if (!Ql(
      l.context._currentValue,
      l.memoizedValue
    ))
      return !0;
    l = l.next;
  }
  return !1;
}
function re(l) {
  be = l, vt = null, l = l.dependencies, l !== null && (l.firstContext = null);
}
function bl(l) {
  return Or(be, l);
}
function Au(l, t) {
  return be === null && re(l), Or(l, t);
}
function Or(l, t) {
  var e = t._currentValue;
  if (t = { context: t, memoizedValue: e, next: null }, vt === null) {
    if (l === null) throw Error(b(308));
    vt = t, l.dependencies = { lanes: 0, firstContext: t }, l.flags |= 524288;
  } else vt = vt.next = t;
  return e;
}
var y0 = typeof AbortController < "u" ? AbortController : function() {
  var l = [], t = this.signal = {
    aborted: !1,
    addEventListener: function(e, a) {
      l.push(a);
    }
  };
  this.abort = function() {
    t.aborted = !0, l.forEach(function(e) {
      return e();
    });
  };
}, v0 = ol.unstable_scheduleCallback, g0 = ol.unstable_NormalPriority, cl = {
  $$typeof: ht,
  Consumer: null,
  Provider: null,
  _currentValue: null,
  _currentValue2: null,
  _threadCount: 0
};
function Ic() {
  return {
    controller: new y0(),
    data: /* @__PURE__ */ new Map(),
    refCount: 0
  };
}
function cu(l) {
  l.refCount--, l.refCount === 0 && v0(g0, function() {
    l.controller.abort();
  });
}
var ja = null, lc = 0, $e = 0, Le = null;
function S0(l, t) {
  if (ja === null) {
    var e = ja = [];
    lc = 0, $e = zf(), Le = {
      status: "pending",
      value: void 0,
      then: function(a) {
        e.push(a);
      }
    };
  }
  return lc++, t.then(hs, hs), t;
}
function hs() {
  if (--lc === 0 && ja !== null) {
    Le !== null && (Le.status = "fulfilled");
    var l = ja;
    ja = null, $e = 0, Le = null;
    for (var t = 0; t < l.length; t++) (0, l[t])();
  }
}
function b0(l, t) {
  var e = [], a = {
    status: "pending",
    value: null,
    reason: null,
    then: function(u) {
      e.push(u);
    }
  };
  return l.then(
    function() {
      a.status = "fulfilled", a.value = t;
      for (var u = 0; u < e.length; u++) (0, e[u])(t);
    },
    function(u) {
      for (a.status = "rejected", a.reason = u, u = 0; u < e.length; u++)
        (0, e[u])(void 0);
    }
  ), a;
}
var ys = C.S;
C.S = function(l, t) {
  Xd = Bl(), typeof t == "object" && t !== null && typeof t.then == "function" && S0(l, t), ys !== null && ys(l, t);
};
var ce = nt(null);
function Pc() {
  var l = ce.current;
  return l !== null ? l : k.pooledCache;
}
function Gu(l, t) {
  t === null ? $(ce, ce.current) : $(ce, t.pool);
}
function Dr() {
  var l = Pc();
  return l === null ? null : { parent: cl._currentValue, pool: l };
}
var fa = Error(b(460)), lf = Error(b(474)), Rn = Error(b(542)), un = { then: function() {
} };
function vs(l) {
  return l = l.status, l === "fulfilled" || l === "rejected";
}
function xr(l, t, e) {
  switch (e = l[e], e === void 0 ? l.push(t) : e !== t && (t.then(yt, yt), t = e), t.status) {
    case "fulfilled":
      return t.value;
    case "rejected":
      throw l = t.reason, Ss(l), l;
    default:
      if (typeof t.status == "string") t.then(yt, yt);
      else {
        if (l = k, l !== null && 100 < l.shellSuspendCounter)
          throw Error(b(482));
        l = t, l.status = "pending", l.then(
          function(a) {
            if (t.status === "pending") {
              var u = t;
              u.status = "fulfilled", u.value = a;
            }
          },
          function(a) {
            if (t.status === "pending") {
              var u = t;
              u.status = "rejected", u.reason = a;
            }
          }
        );
      }
      switch (t.status) {
        case "fulfilled":
          return t.value;
        case "rejected":
          throw l = t.reason, Ss(l), l;
      }
      throw fe = t, fa;
  }
}
function ae(l) {
  try {
    var t = l._init;
    return t(l._payload);
  } catch (e) {
    throw e !== null && typeof e == "object" && typeof e.then == "function" ? (fe = e, fa) : e;
  }
}
var fe = null;
function gs() {
  if (fe === null) throw Error(b(459));
  var l = fe;
  return fe = null, l;
}
function Ss(l) {
  if (l === fa || l === Rn)
    throw Error(b(483));
}
var Ze = null, Ja = 0;
function Mu(l) {
  var t = Ja;
  return Ja += 1, Ze === null && (Ze = []), xr(Ze, l, t);
}
function ga(l, t) {
  t = t.props.ref, l.ref = t !== void 0 ? t : null;
}
function Ou(l, t) {
  throw t.$$typeof === nh ? Error(b(525)) : (l = Object.prototype.toString.call(t), Error(
    b(
      31,
      l === "[object Object]" ? "object with keys {" + Object.keys(t).join(", ") + "}" : l
    )
  ));
}
function Nr(l) {
  function t(r, s) {
    if (l) {
      var m = r.deletions;
      m === null ? (r.deletions = [s], r.flags |= 16) : m.push(s);
    }
  }
  function e(r, s) {
    if (!l) return null;
    for (; s !== null; )
      t(r, s), s = s.sibling;
    return null;
  }
  function a(r) {
    for (var s = /* @__PURE__ */ new Map(); r !== null; )
      r.key !== null ? s.set(r.key, r) : s.set(r.index, r), r = r.sibling;
    return s;
  }
  function u(r, s) {
    return r = gt(r, s), r.index = 0, r.sibling = null, r;
  }
  function n(r, s, m) {
    return r.index = m, l ? (m = r.alternate, m !== null ? (m = m.index, m < s ? (r.flags |= 67108866, s) : m) : (r.flags |= 67108866, s)) : (r.flags |= 1048576, s);
  }
  function i(r) {
    return l && r.alternate === null && (r.flags |= 67108866), r;
  }
  function c(r, s, m, S) {
    return s === null || s.tag !== 6 ? (s = ci(m, r.mode, S), s.return = r, s) : (s = u(s, m), s.return = r, s);
  }
  function f(r, s, m, S) {
    var _ = m.type;
    return _ === Oe ? d(
      r,
      s,
      m.props.children,
      S,
      m.key
    ) : s !== null && (s.elementType === _ || typeof _ == "object" && _ !== null && _.$$typeof === Ot && ae(_) === s.type) ? (s = u(s, m.props), ga(s, m), s.return = r, s) : (s = qu(
      m.type,
      m.key,
      m.props,
      null,
      r.mode,
      S
    ), ga(s, m), s.return = r, s);
  }
  function o(r, s, m, S) {
    return s === null || s.tag !== 4 || s.stateNode.containerInfo !== m.containerInfo || s.stateNode.implementation !== m.implementation ? (s = fi(m, r.mode, S), s.return = r, s) : (s = u(s, m.children || []), s.return = r, s);
  }
  function d(r, s, m, S, _) {
    return s === null || s.tag !== 7 ? (s = ie(
      m,
      r.mode,
      S,
      _
    ), s.return = r, s) : (s = u(s, m), s.return = r, s);
  }
  function y(r, s, m) {
    if (typeof s == "string" && s !== "" || typeof s == "number" || typeof s == "bigint")
      return s = ci(
        "" + s,
        r.mode,
        m
      ), s.return = r, s;
    if (typeof s == "object" && s !== null) {
      switch (s.$$typeof) {
        case bu:
          return m = qu(
            s.type,
            s.key,
            s.props,
            null,
            r.mode,
            m
          ), ga(m, s), m.return = r, m;
        case pa:
          return s = fi(
            s,
            r.mode,
            m
          ), s.return = r, s;
        case Ot:
          return s = ae(s), y(r, s, m);
      }
      if (Ta(s) || ha(s))
        return s = ie(
          s,
          r.mode,
          m,
          null
        ), s.return = r, s;
      if (typeof s.then == "function")
        return y(r, Mu(s), m);
      if (s.$$typeof === ht)
        return y(
          r,
          Au(r, s),
          m
        );
      Ou(r, s);
    }
    return null;
  }
  function h(r, s, m, S) {
    var _ = s !== null ? s.key : null;
    if (typeof m == "string" && m !== "" || typeof m == "number" || typeof m == "bigint")
      return _ !== null ? null : c(r, s, "" + m, S);
    if (typeof m == "object" && m !== null) {
      switch (m.$$typeof) {
        case bu:
          return m.key === _ ? f(r, s, m, S) : null;
        case pa:
          return m.key === _ ? o(r, s, m, S) : null;
        case Ot:
          return m = ae(m), h(r, s, m, S);
      }
      if (Ta(m) || ha(m))
        return _ !== null ? null : d(r, s, m, S, null);
      if (typeof m.then == "function")
        return h(
          r,
          s,
          Mu(m),
          S
        );
      if (m.$$typeof === ht)
        return h(
          r,
          s,
          Au(r, m),
          S
        );
      Ou(r, m);
    }
    return null;
  }
  function v(r, s, m, S, _) {
    if (typeof S == "string" && S !== "" || typeof S == "number" || typeof S == "bigint")
      return r = r.get(m) || null, c(s, r, "" + S, _);
    if (typeof S == "object" && S !== null) {
      switch (S.$$typeof) {
        case bu:
          return r = r.get(
            S.key === null ? m : S.key
          ) || null, f(s, r, S, _);
        case pa:
          return r = r.get(
            S.key === null ? m : S.key
          ) || null, o(s, r, S, _);
        case Ot:
          return S = ae(S), v(
            r,
            s,
            m,
            S,
            _
          );
      }
      if (Ta(S) || ha(S))
        return r = r.get(m) || null, d(s, r, S, _, null);
      if (typeof S.then == "function")
        return v(
          r,
          s,
          m,
          Mu(S),
          _
        );
      if (S.$$typeof === ht)
        return v(
          r,
          s,
          m,
          Au(s, S),
          _
        );
      Ou(s, S);
    }
    return null;
  }
  function p(r, s, m, S) {
    for (var _ = null, O = null, z = s, x = s = 0, j = null; z !== null && x < m.length; x++) {
      z.index > x ? (j = z, z = null) : j = z.sibling;
      var U = h(
        r,
        z,
        m[x],
        S
      );
      if (U === null) {
        z === null && (z = j);
        break;
      }
      l && z && U.alternate === null && t(r, z), s = n(U, s, x), O === null ? _ = U : O.sibling = U, O = U, z = j;
    }
    if (x === m.length)
      return e(r, z), Q && dt(r, x), _;
    if (z === null) {
      for (; x < m.length; x++)
        z = y(r, m[x], S), z !== null && (s = n(
          z,
          s,
          x
        ), O === null ? _ = z : O.sibling = z, O = z);
      return Q && dt(r, x), _;
    }
    for (z = a(z); x < m.length; x++)
      j = v(
        z,
        r,
        x,
        m[x],
        S
      ), j !== null && (l && j.alternate !== null && z.delete(
        j.key === null ? x : j.key
      ), s = n(
        j,
        s,
        x
      ), O === null ? _ = j : O.sibling = j, O = j);
    return l && z.forEach(function(B) {
      return t(r, B);
    }), Q && dt(r, x), _;
  }
  function T(r, s, m, S) {
    if (m == null) throw Error(b(151));
    for (var _ = null, O = null, z = s, x = s = 0, j = null, U = m.next(); z !== null && !U.done; x++, U = m.next()) {
      z.index > x ? (j = z, z = null) : j = z.sibling;
      var B = h(r, z, U.value, S);
      if (B === null) {
        z === null && (z = j);
        break;
      }
      l && z && B.alternate === null && t(r, z), s = n(B, s, x), O === null ? _ = B : O.sibling = B, O = B, z = j;
    }
    if (U.done)
      return e(r, z), Q && dt(r, x), _;
    if (z === null) {
      for (; !U.done; x++, U = m.next())
        U = y(r, U.value, S), U !== null && (s = n(U, s, x), O === null ? _ = U : O.sibling = U, O = U);
      return Q && dt(r, x), _;
    }
    for (z = a(z); !U.done; x++, U = m.next())
      U = v(z, r, x, U.value, S), U !== null && (l && U.alternate !== null && z.delete(U.key === null ? x : U.key), s = n(U, s, x), O === null ? _ = U : O.sibling = U, O = U);
    return l && z.forEach(function(rl) {
      return t(r, rl);
    }), Q && dt(r, x), _;
  }
  function E(r, s, m, S) {
    if (typeof m == "object" && m !== null && m.type === Oe && m.key === null && (m = m.props.children), typeof m == "object" && m !== null) {
      switch (m.$$typeof) {
        case bu:
          l: {
            for (var _ = m.key; s !== null; ) {
              if (s.key === _) {
                if (_ = m.type, _ === Oe) {
                  if (s.tag === 7) {
                    e(
                      r,
                      s.sibling
                    ), S = u(
                      s,
                      m.props.children
                    ), S.return = r, r = S;
                    break l;
                  }
                } else if (s.elementType === _ || typeof _ == "object" && _ !== null && _.$$typeof === Ot && ae(_) === s.type) {
                  e(
                    r,
                    s.sibling
                  ), S = u(s, m.props), ga(S, m), S.return = r, r = S;
                  break l;
                }
                e(r, s);
                break;
              } else t(r, s);
              s = s.sibling;
            }
            m.type === Oe ? (S = ie(
              m.props.children,
              r.mode,
              S,
              m.key
            ), S.return = r, r = S) : (S = qu(
              m.type,
              m.key,
              m.props,
              null,
              r.mode,
              S
            ), ga(S, m), S.return = r, r = S);
          }
          return i(r);
        case pa:
          l: {
            for (_ = m.key; s !== null; ) {
              if (s.key === _)
                if (s.tag === 4 && s.stateNode.containerInfo === m.containerInfo && s.stateNode.implementation === m.implementation) {
                  e(
                    r,
                    s.sibling
                  ), S = u(s, m.children || []), S.return = r, r = S;
                  break l;
                } else {
                  e(r, s);
                  break;
                }
              else t(r, s);
              s = s.sibling;
            }
            S = fi(m, r.mode, S), S.return = r, r = S;
          }
          return i(r);
        case Ot:
          return m = ae(m), E(
            r,
            s,
            m,
            S
          );
      }
      if (Ta(m))
        return p(
          r,
          s,
          m,
          S
        );
      if (ha(m)) {
        if (_ = ha(m), typeof _ != "function") throw Error(b(150));
        return m = _.call(m), T(
          r,
          s,
          m,
          S
        );
      }
      if (typeof m.then == "function")
        return E(
          r,
          s,
          Mu(m),
          S
        );
      if (m.$$typeof === ht)
        return E(
          r,
          s,
          Au(r, m),
          S
        );
      Ou(r, m);
    }
    return typeof m == "string" && m !== "" || typeof m == "number" || typeof m == "bigint" ? (m = "" + m, s !== null && s.tag === 6 ? (e(r, s.sibling), S = u(s, m), S.return = r, r = S) : (e(r, s), S = ci(m, r.mode, S), S.return = r, r = S), i(r)) : e(r, s);
  }
  return function(r, s, m, S) {
    try {
      Ja = 0;
      var _ = E(
        r,
        s,
        m,
        S
      );
      return Ze = null, _;
    } catch (z) {
      if (z === fa || z === Rn) throw z;
      var O = Hl(29, z, null, r.mode);
      return O.lanes = S, O.return = r, O;
    } finally {
    }
  };
}
var de = Nr(!0), jr = Nr(!1), Dt = !1;
function tf(l) {
  l.updateQueue = {
    baseState: l.memoizedState,
    firstBaseUpdate: null,
    lastBaseUpdate: null,
    shared: { pending: null, lanes: 0, hiddenCallbacks: null },
    callbacks: null
  };
}
function tc(l, t) {
  l = l.updateQueue, t.updateQueue === l && (t.updateQueue = {
    baseState: l.baseState,
    firstBaseUpdate: l.firstBaseUpdate,
    lastBaseUpdate: l.lastBaseUpdate,
    shared: l.shared,
    callbacks: null
  });
}
function Qt(l) {
  return { lane: l, tag: 0, payload: null, callback: null, next: null };
}
function Xt(l, t, e) {
  var a = l.updateQueue;
  if (a === null) return null;
  if (a = a.shared, L & 2) {
    var u = a.pending;
    return u === null ? t.next = t : (t.next = u.next, u.next = t), a.pending = t, t = tn(l), pr(l, null, e), t;
  }
  return Hn(l, a, t, e), tn(l);
}
function Ua(l, t, e) {
  if (t = t.updateQueue, t !== null && (t = t.shared, (e & 4194048) !== 0)) {
    var a = t.lanes;
    a &= l.pendingLanes, e |= a, t.lanes = e, $o(l, e);
  }
}
function oi(l, t) {
  var e = l.updateQueue, a = l.alternate;
  if (a !== null && (a = a.updateQueue, e === a)) {
    var u = null, n = null;
    if (e = e.firstBaseUpdate, e !== null) {
      do {
        var i = {
          lane: e.lane,
          tag: e.tag,
          payload: e.payload,
          callback: null,
          next: null
        };
        n === null ? u = n = i : n = n.next = i, e = e.next;
      } while (e !== null);
      n === null ? u = n = t : n = n.next = t;
    } else u = n = t;
    e = {
      baseState: a.baseState,
      firstBaseUpdate: u,
      lastBaseUpdate: n,
      shared: a.shared,
      callbacks: a.callbacks
    }, l.updateQueue = e;
    return;
  }
  l = e.lastBaseUpdate, l === null ? e.firstBaseUpdate = t : l.next = t, e.lastBaseUpdate = t;
}
var ec = !1;
function Ca() {
  if (ec) {
    var l = Le;
    if (l !== null) throw l;
  }
}
function Ha(l, t, e, a) {
  ec = !1;
  var u = l.updateQueue;
  Dt = !1;
  var n = u.firstBaseUpdate, i = u.lastBaseUpdate, c = u.shared.pending;
  if (c !== null) {
    u.shared.pending = null;
    var f = c, o = f.next;
    f.next = null, i === null ? n = o : i.next = o, i = f;
    var d = l.alternate;
    d !== null && (d = d.updateQueue, c = d.lastBaseUpdate, c !== i && (c === null ? d.firstBaseUpdate = o : c.next = o, d.lastBaseUpdate = f));
  }
  if (n !== null) {
    var y = u.baseState;
    i = 0, d = o = f = null, c = n;
    do {
      var h = c.lane & -536870913, v = h !== c.lane;
      if (v ? (G & h) === h : (a & h) === h) {
        h !== 0 && h === $e && (ec = !0), d !== null && (d = d.next = {
          lane: 0,
          tag: c.tag,
          payload: c.payload,
          callback: null,
          next: null
        });
        l: {
          var p = l, T = c;
          h = t;
          var E = e;
          switch (T.tag) {
            case 1:
              if (p = T.payload, typeof p == "function") {
                y = p.call(E, y, h);
                break l;
              }
              y = p;
              break l;
            case 3:
              p.flags = p.flags & -65537 | 128;
            case 0:
              if (p = T.payload, h = typeof p == "function" ? p.call(E, y, h) : p, h == null) break l;
              y = P({}, y, h);
              break l;
            case 2:
              Dt = !0;
          }
        }
        h = c.callback, h !== null && (l.flags |= 64, v && (l.flags |= 8192), v = u.callbacks, v === null ? u.callbacks = [h] : v.push(h));
      } else
        v = {
          lane: h,
          tag: c.tag,
          payload: c.payload,
          callback: c.callback,
          next: null
        }, d === null ? (o = d = v, f = y) : d = d.next = v, i |= h;
      if (c = c.next, c === null) {
        if (c = u.shared.pending, c === null)
          break;
        v = c, c = v.next, v.next = null, u.lastBaseUpdate = v, u.shared.pending = null;
      }
    } while (!0);
    d === null && (f = y), u.baseState = f, u.firstBaseUpdate = o, u.lastBaseUpdate = d, n === null && (u.shared.lanes = 0), Wt |= i, l.lanes = i, l.memoizedState = y;
  }
}
function Ur(l, t) {
  if (typeof l != "function")
    throw Error(b(191, l));
  l.call(t);
}
function Cr(l, t) {
  var e = l.callbacks;
  if (e !== null)
    for (l.callbacks = null, l = 0; l < e.length; l++)
      Ur(e[l], t);
}
var We = nt(null), nn = nt(0);
function bs(l, t) {
  l = zt, $(nn, l), $(We, t), zt = l | t.baseLanes;
}
function ac() {
  $(nn, zt), $(We, We.current);
}
function ef() {
  zt = nn.current, yl(We), yl(nn);
}
var Xl = nt(null), $l = null;
function Nt(l) {
  var t = l.alternate;
  $(al, al.current & 1), $(Xl, l), $l === null && (t === null || We.current !== null || t.memoizedState !== null) && ($l = l);
}
function uc(l) {
  $(al, al.current), $(Xl, l), $l === null && ($l = l);
}
function Hr(l) {
  l.tag === 22 ? ($(al, al.current), $(Xl, l), $l === null && ($l = l)) : jt();
}
function jt() {
  $(al, al.current), $(Xl, Xl.current);
}
function Cl(l) {
  yl(Xl), $l === l && ($l = null), yl(al);
}
var al = nt(0);
function cn(l) {
  for (var t = l; t !== null; ) {
    if (t.tag === 13) {
      var e = t.memoizedState;
      if (e !== null && (e = e.dehydrated, e === null || Tc(e) || zc(e)))
        return t;
    } else if (t.tag === 19 && (t.memoizedProps.revealOrder === "forwards" || t.memoizedProps.revealOrder === "backwards" || t.memoizedProps.revealOrder === "unstable_legacy-backwards" || t.memoizedProps.revealOrder === "together")) {
      if (t.flags & 128) return t;
    } else if (t.child !== null) {
      t.child.return = t, t = t.child;
      continue;
    }
    if (t === l) break;
    for (; t.sibling === null; ) {
      if (t.return === null || t.return === l) return null;
      t = t.return;
    }
    t.sibling.return = t.return, t = t.sibling;
  }
  return null;
}
var Et = 0, R = null, J = null, nl = null, fn = !1, we = !1, me = !1, sn = 0, ka = 0, Ve = null, _0 = 0;
function tl() {
  throw Error(b(321));
}
function af(l, t) {
  if (t === null) return !1;
  for (var e = 0; e < t.length && e < l.length; e++)
    if (!Ql(l[e], t[e])) return !1;
  return !0;
}
function uf(l, t, e, a, u, n) {
  return Et = n, R = t, t.memoizedState = null, t.updateQueue = null, t.lanes = 0, C.H = l === null || l.memoizedState === null ? rd : vf, me = !1, n = e(a, u), me = !1, we && (n = Br(
    t,
    e,
    a,
    u
  )), Rr(l), n;
}
function Rr(l) {
  C.H = $a;
  var t = J !== null && J.next !== null;
  if (Et = 0, nl = J = R = null, fn = !1, ka = 0, Ve = null, t) throw Error(b(300));
  l === null || fl || (l = l.dependencies, l !== null && an(l) && (fl = !0));
}
function Br(l, t, e, a) {
  R = l;
  var u = 0;
  do {
    if (we && (Ve = null), ka = 0, we = !1, 25 <= u) throw Error(b(301));
    if (u += 1, nl = J = null, l.updateQueue != null) {
      var n = l.updateQueue;
      n.lastEffect = null, n.events = null, n.stores = null, n.memoCache != null && (n.memoCache.index = 0);
    }
    C.H = dd, n = t(e, a);
  } while (we);
  return n;
}
function E0() {
  var l = C.H, t = l.useState()[0];
  return t = typeof t.then == "function" ? fu(t) : t, l = l.useState()[0], (J !== null ? J.memoizedState : null) !== l && (R.flags |= 1024), t;
}
function nf() {
  var l = sn !== 0;
  return sn = 0, l;
}
function cf(l, t, e) {
  t.updateQueue = l.updateQueue, t.flags &= -2053, l.lanes &= ~e;
}
function ff(l) {
  if (fn) {
    for (l = l.memoizedState; l !== null; ) {
      var t = l.queue;
      t !== null && (t.pending = null), l = l.next;
    }
    fn = !1;
  }
  Et = 0, nl = J = R = null, we = !1, ka = sn = 0, Ve = null;
}
function pl() {
  var l = {
    memoizedState: null,
    baseState: null,
    baseQueue: null,
    queue: null,
    next: null
  };
  return nl === null ? R.memoizedState = nl = l : nl = nl.next = l, nl;
}
function ul() {
  if (J === null) {
    var l = R.alternate;
    l = l !== null ? l.memoizedState : null;
  } else l = J.next;
  var t = nl === null ? R.memoizedState : nl.next;
  if (t !== null)
    nl = t, J = l;
  else {
    if (l === null)
      throw R.alternate === null ? Error(b(467)) : Error(b(310));
    J = l, l = {
      memoizedState: J.memoizedState,
      baseState: J.baseState,
      baseQueue: J.baseQueue,
      queue: J.queue,
      next: null
    }, nl === null ? R.memoizedState = nl = l : nl = nl.next = l;
  }
  return nl;
}
function Bn() {
  return { lastEffect: null, events: null, stores: null, memoCache: null };
}
function fu(l) {
  var t = ka;
  return ka += 1, Ve === null && (Ve = []), l = xr(Ve, l, t), t = R, (nl === null ? t.memoizedState : nl.next) === null && (t = t.alternate, C.H = t === null || t.memoizedState === null ? rd : vf), l;
}
function Yn(l) {
  if (l !== null && typeof l == "object") {
    if (typeof l.then == "function") return fu(l);
    if (l.$$typeof === ht) return bl(l);
  }
  throw Error(b(438, String(l)));
}
function sf(l) {
  var t = null, e = R.updateQueue;
  if (e !== null && (t = e.memoCache), t == null) {
    var a = R.alternate;
    a !== null && (a = a.updateQueue, a !== null && (a = a.memoCache, a != null && (t = {
      data: a.data.map(function(u) {
        return u.slice();
      }),
      index: 0
    })));
  }
  if (t == null && (t = { data: [], index: 0 }), e === null && (e = Bn(), R.updateQueue = e), e.memoCache = t, e = t.data[t.index], e === void 0)
    for (e = t.data[t.index] = Array(l), a = 0; a < l; a++)
      e[a] = ih;
  return t.index++, e;
}
function pt(l, t) {
  return typeof t == "function" ? t(l) : t;
}
function Qu(l) {
  var t = ul();
  return of(t, J, l);
}
function of(l, t, e) {
  var a = l.queue;
  if (a === null) throw Error(b(311));
  a.lastRenderedReducer = e;
  var u = l.baseQueue, n = a.pending;
  if (n !== null) {
    if (u !== null) {
      var i = u.next;
      u.next = n.next, n.next = i;
    }
    t.baseQueue = u = n, a.pending = null;
  }
  if (n = l.baseState, u === null) l.memoizedState = n;
  else {
    t = u.next;
    var c = i = null, f = null, o = t, d = !1;
    do {
      var y = o.lane & -536870913;
      if (y !== o.lane ? (G & y) === y : (Et & y) === y) {
        var h = o.revertLane;
        if (h === 0)
          f !== null && (f = f.next = {
            lane: 0,
            revertLane: 0,
            gesture: null,
            action: o.action,
            hasEagerState: o.hasEagerState,
            eagerState: o.eagerState,
            next: null
          }), y === $e && (d = !0);
        else if ((Et & h) === h) {
          o = o.next, h === $e && (d = !0);
          continue;
        } else
          y = {
            lane: 0,
            revertLane: o.revertLane,
            gesture: null,
            action: o.action,
            hasEagerState: o.hasEagerState,
            eagerState: o.eagerState,
            next: null
          }, f === null ? (c = f = y, i = n) : f = f.next = y, R.lanes |= h, Wt |= h;
        y = o.action, me && e(n, y), n = o.hasEagerState ? o.eagerState : e(n, y);
      } else
        h = {
          lane: y,
          revertLane: o.revertLane,
          gesture: o.gesture,
          action: o.action,
          hasEagerState: o.hasEagerState,
          eagerState: o.eagerState,
          next: null
        }, f === null ? (c = f = h, i = n) : f = f.next = h, R.lanes |= y, Wt |= y;
      o = o.next;
    } while (o !== null && o !== t);
    if (f === null ? i = n : f.next = c, !Ql(n, l.memoizedState) && (fl = !0, d && (e = Le, e !== null)))
      throw e;
    l.memoizedState = n, l.baseState = i, l.baseQueue = f, a.lastRenderedState = n;
  }
  return u === null && (a.lanes = 0), [l.memoizedState, a.dispatch];
}
function ri(l) {
  var t = ul(), e = t.queue;
  if (e === null) throw Error(b(311));
  e.lastRenderedReducer = l;
  var a = e.dispatch, u = e.pending, n = t.memoizedState;
  if (u !== null) {
    e.pending = null;
    var i = u = u.next;
    do
      n = l(n, i.action), i = i.next;
    while (i !== u);
    Ql(n, t.memoizedState) || (fl = !0), t.memoizedState = n, t.baseQueue === null && (t.baseState = n), e.lastRenderedState = n;
  }
  return [n, a];
}
function Yr(l, t, e) {
  var a = R, u = ul(), n = Q;
  if (n) {
    if (e === void 0) throw Error(b(407));
    e = e();
  } else e = t();
  var i = !Ql(
    (J || u).memoizedState,
    e
  );
  if (i && (u.memoizedState = e, fl = !0), u = u.queue, rf(Qr.bind(null, a, u, l), [
    l
  ]), u.getSnapshot !== t || i || nl !== null && nl.memoizedState.tag & 1) {
    if (a.flags |= 2048, Fe(
      9,
      { destroy: void 0 },
      Gr.bind(
        null,
        a,
        u,
        e,
        t
      ),
      null
    ), k === null) throw Error(b(349));
    n || Et & 127 || qr(a, t, e);
  }
  return e;
}
function qr(l, t, e) {
  l.flags |= 16384, l = { getSnapshot: t, value: e }, t = R.updateQueue, t === null ? (t = Bn(), R.updateQueue = t, t.stores = [l]) : (e = t.stores, e === null ? t.stores = [l] : e.push(l));
}
function Gr(l, t, e, a) {
  t.value = e, t.getSnapshot = a, Xr(t) && Lr(l);
}
function Qr(l, t, e) {
  return e(function() {
    Xr(t) && Lr(l);
  });
}
function Xr(l) {
  var t = l.getSnapshot;
  l = l.value;
  try {
    var e = t();
    return !Ql(l, e);
  } catch {
    return !0;
  }
}
function Lr(l) {
  var t = Se(l, 2);
  t !== null && xl(t, l, 2);
}
function nc(l) {
  var t = pl();
  if (typeof l == "function") {
    var e = l;
    if (l = e(), me) {
      Ct(!0);
      try {
        e();
      } finally {
        Ct(!1);
      }
    }
  }
  return t.memoizedState = t.baseState = l, t.queue = {
    pending: null,
    lanes: 0,
    dispatch: null,
    lastRenderedReducer: pt,
    lastRenderedState: l
  }, t;
}
function Zr(l, t, e, a) {
  return l.baseState = e, of(
    l,
    J,
    typeof a == "function" ? a : pt
  );
}
function p0(l, t, e, a, u) {
  if (Gn(l)) throw Error(b(485));
  if (l = t.action, l !== null) {
    var n = {
      payload: u,
      action: l,
      next: null,
      isTransition: !0,
      status: "pending",
      value: null,
      reason: null,
      listeners: [],
      then: function(i) {
        n.listeners.push(i);
      }
    };
    C.T !== null ? e(!0) : n.isTransition = !1, a(n), e = t.pending, e === null ? (n.next = t.pending = n, wr(t, n)) : (n.next = e.next, t.pending = e.next = n);
  }
}
function wr(l, t) {
  var e = t.action, a = t.payload, u = l.state;
  if (t.isTransition) {
    var n = C.T, i = {};
    C.T = i;
    try {
      var c = e(u, a), f = C.S;
      f !== null && f(i, c), _s(l, t, c);
    } catch (o) {
      ic(l, t, o);
    } finally {
      n !== null && i.types !== null && (n.types = i.types), C.T = n;
    }
  } else
    try {
      n = e(u, a), _s(l, t, n);
    } catch (o) {
      ic(l, t, o);
    }
}
function _s(l, t, e) {
  e !== null && typeof e == "object" && typeof e.then == "function" ? e.then(
    function(a) {
      Es(l, t, a);
    },
    function(a) {
      return ic(l, t, a);
    }
  ) : Es(l, t, e);
}
function Es(l, t, e) {
  t.status = "fulfilled", t.value = e, Vr(t), l.state = e, t = l.pending, t !== null && (e = t.next, e === t ? l.pending = null : (e = e.next, t.next = e, wr(l, e)));
}
function ic(l, t, e) {
  var a = l.pending;
  if (l.pending = null, a !== null) {
    a = a.next;
    do
      t.status = "rejected", t.reason = e, Vr(t), t = t.next;
    while (t !== a);
  }
  l.action = null;
}
function Vr(l) {
  l = l.listeners;
  for (var t = 0; t < l.length; t++) (0, l[t])();
}
function Kr(l, t) {
  return t;
}
function ps(l, t) {
  if (Q) {
    var e = k.formState;
    if (e !== null) {
      l: {
        var a = R;
        if (Q) {
          if (F) {
            t: {
              for (var u = F, n = kl; u.nodeType !== 8; ) {
                if (!n) {
                  u = null;
                  break t;
                }
                if (u = Wl(
                  u.nextSibling
                ), u === null) {
                  u = null;
                  break t;
                }
              }
              n = u.data, u = n === "F!" || n === "F" ? u : null;
            }
            if (u) {
              F = Wl(
                u.nextSibling
              ), a = u.data === "F!";
              break l;
            }
          }
          kt(a);
        }
        a = !1;
      }
      a && (t = e[0]);
    }
  }
  return e = pl(), e.memoizedState = e.baseState = t, a = {
    pending: null,
    lanes: 0,
    dispatch: null,
    lastRenderedReducer: Kr,
    lastRenderedState: t
  }, e.queue = a, e = fd.bind(
    null,
    R,
    a
  ), a.dispatch = e, a = nc(!1), n = yf.bind(
    null,
    R,
    !1,
    a.queue
  ), a = pl(), u = {
    state: t,
    dispatch: null,
    action: l,
    pending: null
  }, a.queue = u, e = p0.bind(
    null,
    R,
    u,
    n,
    e
  ), u.dispatch = e, a.memoizedState = l, [t, e, !1];
}
function Ts(l) {
  var t = ul();
  return Jr(t, J, l);
}
function Jr(l, t, e) {
  if (t = of(
    l,
    t,
    Kr
  )[0], l = Qu(pt)[0], typeof t == "object" && t !== null && typeof t.then == "function")
    try {
      var a = fu(t);
    } catch (i) {
      throw i === fa ? Rn : i;
    }
  else a = t;
  t = ul();
  var u = t.queue, n = u.dispatch;
  return e !== t.memoizedState && (R.flags |= 2048, Fe(
    9,
    { destroy: void 0 },
    T0.bind(null, u, e),
    null
  )), [a, n, l];
}
function T0(l, t) {
  l.action = t;
}
function zs(l) {
  var t = ul(), e = J;
  if (e !== null)
    return Jr(t, e, l);
  ul(), t = t.memoizedState, e = ul();
  var a = e.queue.dispatch;
  return e.memoizedState = l, [t, a, !1];
}
function Fe(l, t, e, a) {
  return l = { tag: l, create: e, deps: a, inst: t, next: null }, t = R.updateQueue, t === null && (t = Bn(), R.updateQueue = t), e = t.lastEffect, e === null ? t.lastEffect = l.next = l : (a = e.next, e.next = l, l.next = a, t.lastEffect = l), l;
}
function kr() {
  return ul().memoizedState;
}
function Xu(l, t, e, a) {
  var u = pl();
  R.flags |= l, u.memoizedState = Fe(
    1 | t,
    { destroy: void 0 },
    e,
    a === void 0 ? null : a
  );
}
function qn(l, t, e, a) {
  var u = ul();
  a = a === void 0 ? null : a;
  var n = u.memoizedState.inst;
  J !== null && a !== null && af(a, J.memoizedState.deps) ? u.memoizedState = Fe(t, n, e, a) : (R.flags |= l, u.memoizedState = Fe(
    1 | t,
    n,
    e,
    a
  ));
}
function As(l, t) {
  Xu(8390656, 8, l, t);
}
function rf(l, t) {
  qn(2048, 8, l, t);
}
function z0(l) {
  R.flags |= 4;
  var t = R.updateQueue;
  if (t === null)
    t = Bn(), R.updateQueue = t, t.events = [l];
  else {
    var e = t.events;
    e === null ? t.events = [l] : e.push(l);
  }
}
function $r(l) {
  var t = ul().memoizedState;
  return z0({ ref: t, nextImpl: l }), function() {
    if (L & 2) throw Error(b(440));
    return t.impl.apply(void 0, arguments);
  };
}
function Wr(l, t) {
  return qn(4, 2, l, t);
}
function Fr(l, t) {
  return qn(4, 4, l, t);
}
function Ir(l, t) {
  if (typeof t == "function") {
    l = l();
    var e = t(l);
    return function() {
      typeof e == "function" ? e() : t(null);
    };
  }
  if (t != null)
    return l = l(), t.current = l, function() {
      t.current = null;
    };
}
function Pr(l, t, e) {
  e = e != null ? e.concat([l]) : null, qn(4, 4, Ir.bind(null, t, l), e);
}
function df() {
}
function ld(l, t) {
  var e = ul();
  t = t === void 0 ? null : t;
  var a = e.memoizedState;
  return t !== null && af(t, a[1]) ? a[0] : (e.memoizedState = [l, t], l);
}
function td(l, t) {
  var e = ul();
  t = t === void 0 ? null : t;
  var a = e.memoizedState;
  if (t !== null && af(t, a[1]))
    return a[0];
  if (a = l(), me) {
    Ct(!0);
    try {
      l();
    } finally {
      Ct(!1);
    }
  }
  return e.memoizedState = [a, t], a;
}
function mf(l, t, e) {
  return e === void 0 || Et & 1073741824 && !(G & 261930) ? l.memoizedState = t : (l.memoizedState = e, l = Zd(), R.lanes |= l, Wt |= l, e);
}
function ed(l, t, e, a) {
  return Ql(e, t) ? e : We.current !== null ? (l = mf(l, e, a), Ql(l, t) || (fl = !0), l) : !(Et & 42) || Et & 1073741824 && !(G & 261930) ? (fl = !0, l.memoizedState = e) : (l = Zd(), R.lanes |= l, Wt |= l, t);
}
function ad(l, t, e, a, u) {
  var n = Z.p;
  Z.p = n !== 0 && 8 > n ? n : 8;
  var i = C.T, c = {};
  C.T = c, yf(l, !1, t, e);
  try {
    var f = u(), o = C.S;
    if (o !== null && o(c, f), f !== null && typeof f == "object" && typeof f.then == "function") {
      var d = b0(
        f,
        a
      );
      Ra(
        l,
        t,
        d,
        Gl(l)
      );
    } else
      Ra(
        l,
        t,
        a,
        Gl(l)
      );
  } catch (y) {
    Ra(
      l,
      t,
      { then: function() {
      }, status: "rejected", reason: y },
      Gl()
    );
  } finally {
    Z.p = n, i !== null && c.types !== null && (i.types = c.types), C.T = i;
  }
}
function A0() {
}
function cc(l, t, e, a) {
  if (l.tag !== 5) throw Error(b(476));
  var u = ud(l).queue;
  ad(
    l,
    u,
    t,
    ne,
    e === null ? A0 : function() {
      return nd(l), e(a);
    }
  );
}
function ud(l) {
  var t = l.memoizedState;
  if (t !== null) return t;
  t = {
    memoizedState: ne,
    baseState: ne,
    baseQueue: null,
    queue: {
      pending: null,
      lanes: 0,
      dispatch: null,
      lastRenderedReducer: pt,
      lastRenderedState: ne
    },
    next: null
  };
  var e = {};
  return t.next = {
    memoizedState: e,
    baseState: e,
    baseQueue: null,
    queue: {
      pending: null,
      lanes: 0,
      dispatch: null,
      lastRenderedReducer: pt,
      lastRenderedState: e
    },
    next: null
  }, l.memoizedState = t, l = l.alternate, l !== null && (l.memoizedState = t), t;
}
function nd(l) {
  var t = ud(l);
  t.next === null && (t = l.alternate.memoizedState), Ra(
    l,
    t.next.queue,
    {},
    Gl()
  );
}
function hf() {
  return bl(Ia);
}
function id() {
  return ul().memoizedState;
}
function cd() {
  return ul().memoizedState;
}
function M0(l) {
  for (var t = l.return; t !== null; ) {
    switch (t.tag) {
      case 24:
      case 3:
        var e = Gl();
        l = Qt(e);
        var a = Xt(t, l, e);
        a !== null && (xl(a, t, e), Ua(a, t, e)), t = { cache: Ic() }, l.payload = t;
        return;
    }
    t = t.return;
  }
}
function O0(l, t, e) {
  var a = Gl();
  e = {
    lane: a,
    revertLane: 0,
    gesture: null,
    action: e,
    hasEagerState: !1,
    eagerState: null,
    next: null
  }, Gn(l) ? sd(t, e) : (e = kc(l, t, e, a), e !== null && (xl(e, l, a), od(e, t, a)));
}
function fd(l, t, e) {
  var a = Gl();
  Ra(l, t, e, a);
}
function Ra(l, t, e, a) {
  var u = {
    lane: a,
    revertLane: 0,
    gesture: null,
    action: e,
    hasEagerState: !1,
    eagerState: null,
    next: null
  };
  if (Gn(l)) sd(t, u);
  else {
    var n = l.alternate;
    if (l.lanes === 0 && (n === null || n.lanes === 0) && (n = t.lastRenderedReducer, n !== null))
      try {
        var i = t.lastRenderedState, c = n(i, e);
        if (u.hasEagerState = !0, u.eagerState = c, Ql(c, i))
          return Hn(l, t, u, 0), k === null && Cn(), !1;
      } catch {
      } finally {
      }
    if (e = kc(l, t, u, a), e !== null)
      return xl(e, l, a), od(e, t, a), !0;
  }
  return !1;
}
function yf(l, t, e, a) {
  if (a = {
    lane: 2,
    revertLane: zf(),
    gesture: null,
    action: a,
    hasEagerState: !1,
    eagerState: null,
    next: null
  }, Gn(l)) {
    if (t) throw Error(b(479));
  } else
    t = kc(
      l,
      e,
      a,
      2
    ), t !== null && xl(t, l, 2);
}
function Gn(l) {
  var t = l.alternate;
  return l === R || t !== null && t === R;
}
function sd(l, t) {
  we = fn = !0;
  var e = l.pending;
  e === null ? t.next = t : (t.next = e.next, e.next = t), l.pending = t;
}
function od(l, t, e) {
  if (e & 4194048) {
    var a = t.lanes;
    a &= l.pendingLanes, e |= a, t.lanes = e, $o(l, e);
  }
}
var $a = {
  readContext: bl,
  use: Yn,
  useCallback: tl,
  useContext: tl,
  useEffect: tl,
  useImperativeHandle: tl,
  useLayoutEffect: tl,
  useInsertionEffect: tl,
  useMemo: tl,
  useReducer: tl,
  useRef: tl,
  useState: tl,
  useDebugValue: tl,
  useDeferredValue: tl,
  useTransition: tl,
  useSyncExternalStore: tl,
  useId: tl,
  useHostTransitionStatus: tl,
  useFormState: tl,
  useActionState: tl,
  useOptimistic: tl,
  useMemoCache: tl,
  useCacheRefresh: tl
};
$a.useEffectEvent = tl;
var rd = {
  readContext: bl,
  use: Yn,
  useCallback: function(l, t) {
    return pl().memoizedState = [
      l,
      t === void 0 ? null : t
    ], l;
  },
  useContext: bl,
  useEffect: As,
  useImperativeHandle: function(l, t, e) {
    e = e != null ? e.concat([l]) : null, Xu(
      4194308,
      4,
      Ir.bind(null, t, l),
      e
    );
  },
  useLayoutEffect: function(l, t) {
    return Xu(4194308, 4, l, t);
  },
  useInsertionEffect: function(l, t) {
    Xu(4, 2, l, t);
  },
  useMemo: function(l, t) {
    var e = pl();
    t = t === void 0 ? null : t;
    var a = l();
    if (me) {
      Ct(!0);
      try {
        l();
      } finally {
        Ct(!1);
      }
    }
    return e.memoizedState = [a, t], a;
  },
  useReducer: function(l, t, e) {
    var a = pl();
    if (e !== void 0) {
      var u = e(t);
      if (me) {
        Ct(!0);
        try {
          e(t);
        } finally {
          Ct(!1);
        }
      }
    } else u = t;
    return a.memoizedState = a.baseState = u, l = {
      pending: null,
      lanes: 0,
      dispatch: null,
      lastRenderedReducer: l,
      lastRenderedState: u
    }, a.queue = l, l = l.dispatch = O0.bind(
      null,
      R,
      l
    ), [a.memoizedState, l];
  },
  useRef: function(l) {
    var t = pl();
    return l = { current: l }, t.memoizedState = l;
  },
  useState: function(l) {
    l = nc(l);
    var t = l.queue, e = fd.bind(null, R, t);
    return t.dispatch = e, [l.memoizedState, e];
  },
  useDebugValue: df,
  useDeferredValue: function(l, t) {
    var e = pl();
    return mf(e, l, t);
  },
  useTransition: function() {
    var l = nc(!1);
    return l = ad.bind(
      null,
      R,
      l.queue,
      !0,
      !1
    ), pl().memoizedState = l, [!1, l];
  },
  useSyncExternalStore: function(l, t, e) {
    var a = R, u = pl();
    if (Q) {
      if (e === void 0)
        throw Error(b(407));
      e = e();
    } else {
      if (e = t(), k === null)
        throw Error(b(349));
      G & 127 || qr(a, t, e);
    }
    u.memoizedState = e;
    var n = { value: e, getSnapshot: t };
    return u.queue = n, As(Qr.bind(null, a, n, l), [
      l
    ]), a.flags |= 2048, Fe(
      9,
      { destroy: void 0 },
      Gr.bind(
        null,
        a,
        n,
        e,
        t
      ),
      null
    ), e;
  },
  useId: function() {
    var l = pl(), t = k.identifierPrefix;
    if (Q) {
      var e = et, a = tt;
      e = (a & ~(1 << 32 - ql(a) - 1)).toString(32) + e, t = "_" + t + "R_" + e, e = sn++, 0 < e && (t += "H" + e.toString(32)), t += "_";
    } else
      e = _0++, t = "_" + t + "r_" + e.toString(32) + "_";
    return l.memoizedState = t;
  },
  useHostTransitionStatus: hf,
  useFormState: ps,
  useActionState: ps,
  useOptimistic: function(l) {
    var t = pl();
    t.memoizedState = t.baseState = l;
    var e = {
      pending: null,
      lanes: 0,
      dispatch: null,
      lastRenderedReducer: null,
      lastRenderedState: null
    };
    return t.queue = e, t = yf.bind(
      null,
      R,
      !0,
      e
    ), e.dispatch = t, [l, t];
  },
  useMemoCache: sf,
  useCacheRefresh: function() {
    return pl().memoizedState = M0.bind(
      null,
      R
    );
  },
  useEffectEvent: function(l) {
    var t = pl(), e = { impl: l };
    return t.memoizedState = e, function() {
      if (L & 2)
        throw Error(b(440));
      return e.impl.apply(void 0, arguments);
    };
  }
}, vf = {
  readContext: bl,
  use: Yn,
  useCallback: ld,
  useContext: bl,
  useEffect: rf,
  useImperativeHandle: Pr,
  useInsertionEffect: Wr,
  useLayoutEffect: Fr,
  useMemo: td,
  useReducer: Qu,
  useRef: kr,
  useState: function() {
    return Qu(pt);
  },
  useDebugValue: df,
  useDeferredValue: function(l, t) {
    var e = ul();
    return ed(
      e,
      J.memoizedState,
      l,
      t
    );
  },
  useTransition: function() {
    var l = Qu(pt)[0], t = ul().memoizedState;
    return [
      typeof l == "boolean" ? l : fu(l),
      t
    ];
  },
  useSyncExternalStore: Yr,
  useId: id,
  useHostTransitionStatus: hf,
  useFormState: Ts,
  useActionState: Ts,
  useOptimistic: function(l, t) {
    var e = ul();
    return Zr(e, J, l, t);
  },
  useMemoCache: sf,
  useCacheRefresh: cd
};
vf.useEffectEvent = $r;
var dd = {
  readContext: bl,
  use: Yn,
  useCallback: ld,
  useContext: bl,
  useEffect: rf,
  useImperativeHandle: Pr,
  useInsertionEffect: Wr,
  useLayoutEffect: Fr,
  useMemo: td,
  useReducer: ri,
  useRef: kr,
  useState: function() {
    return ri(pt);
  },
  useDebugValue: df,
  useDeferredValue: function(l, t) {
    var e = ul();
    return J === null ? mf(e, l, t) : ed(
      e,
      J.memoizedState,
      l,
      t
    );
  },
  useTransition: function() {
    var l = ri(pt)[0], t = ul().memoizedState;
    return [
      typeof l == "boolean" ? l : fu(l),
      t
    ];
  },
  useSyncExternalStore: Yr,
  useId: id,
  useHostTransitionStatus: hf,
  useFormState: zs,
  useActionState: zs,
  useOptimistic: function(l, t) {
    var e = ul();
    return J !== null ? Zr(e, J, l, t) : (e.baseState = l, [l, e.queue.dispatch]);
  },
  useMemoCache: sf,
  useCacheRefresh: cd
};
dd.useEffectEvent = $r;
function di(l, t, e, a) {
  t = l.memoizedState, e = e(a, t), e = e == null ? t : P({}, t, e), l.memoizedState = e, l.lanes === 0 && (l.updateQueue.baseState = e);
}
var fc = {
  enqueueSetState: function(l, t, e) {
    l = l._reactInternals;
    var a = Gl(), u = Qt(a);
    u.payload = t, e != null && (u.callback = e), t = Xt(l, u, a), t !== null && (xl(t, l, a), Ua(t, l, a));
  },
  enqueueReplaceState: function(l, t, e) {
    l = l._reactInternals;
    var a = Gl(), u = Qt(a);
    u.tag = 1, u.payload = t, e != null && (u.callback = e), t = Xt(l, u, a), t !== null && (xl(t, l, a), Ua(t, l, a));
  },
  enqueueForceUpdate: function(l, t) {
    l = l._reactInternals;
    var e = Gl(), a = Qt(e);
    a.tag = 2, t != null && (a.callback = t), t = Xt(l, a, e), t !== null && (xl(t, l, e), Ua(t, l, e));
  }
};
function Ms(l, t, e, a, u, n, i) {
  return l = l.stateNode, typeof l.shouldComponentUpdate == "function" ? l.shouldComponentUpdate(a, n, i) : t.prototype && t.prototype.isPureReactComponent ? !wa(e, a) || !wa(u, n) : !0;
}
function Os(l, t, e, a) {
  l = t.state, typeof t.componentWillReceiveProps == "function" && t.componentWillReceiveProps(e, a), typeof t.UNSAFE_componentWillReceiveProps == "function" && t.UNSAFE_componentWillReceiveProps(e, a), t.state !== l && fc.enqueueReplaceState(t, t.state, null);
}
function he(l, t) {
  var e = t;
  if ("ref" in t) {
    e = {};
    for (var a in t)
      a !== "ref" && (e[a] = t[a]);
  }
  if (l = l.defaultProps) {
    e === t && (e = P({}, e));
    for (var u in l)
      e[u] === void 0 && (e[u] = l[u]);
  }
  return e;
}
function md(l) {
  ln(l);
}
function hd(l) {
  console.error(l);
}
function yd(l) {
  ln(l);
}
function on(l, t) {
  try {
    var e = l.onUncaughtError;
    e(t.value, { componentStack: t.stack });
  } catch (a) {
    setTimeout(function() {
      throw a;
    });
  }
}
function Ds(l, t, e) {
  try {
    var a = l.onCaughtError;
    a(e.value, {
      componentStack: e.stack,
      errorBoundary: t.tag === 1 ? t.stateNode : null
    });
  } catch (u) {
    setTimeout(function() {
      throw u;
    });
  }
}
function sc(l, t, e) {
  return e = Qt(e), e.tag = 3, e.payload = { element: null }, e.callback = function() {
    on(l, t);
  }, e;
}
function vd(l) {
  return l = Qt(l), l.tag = 3, l;
}
function gd(l, t, e, a) {
  var u = e.type.getDerivedStateFromError;
  if (typeof u == "function") {
    var n = a.value;
    l.payload = function() {
      return u(n);
    }, l.callback = function() {
      Ds(t, e, a);
    };
  }
  var i = e.stateNode;
  i !== null && typeof i.componentDidCatch == "function" && (l.callback = function() {
    Ds(t, e, a), typeof u != "function" && (Lt === null ? Lt = /* @__PURE__ */ new Set([this]) : Lt.add(this));
    var c = a.stack;
    this.componentDidCatch(a.value, {
      componentStack: c !== null ? c : ""
    });
  });
}
function D0(l, t, e, a, u) {
  if (e.flags |= 32768, a !== null && typeof a == "object" && typeof a.then == "function") {
    if (t = e.alternate, t !== null && ca(
      t,
      e,
      u,
      !0
    ), e = Xl.current, e !== null) {
      switch (e.tag) {
        case 31:
        case 13:
          return $l === null ? yn() : e.alternate === null && el === 0 && (el = 3), e.flags &= -257, e.flags |= 65536, e.lanes = u, a === un ? e.flags |= 16384 : (t = e.updateQueue, t === null ? e.updateQueue = /* @__PURE__ */ new Set([a]) : t.add(a), Ti(l, a, u)), !1;
        case 22:
          return e.flags |= 65536, a === un ? e.flags |= 16384 : (t = e.updateQueue, t === null ? (t = {
            transitions: null,
            markerInstances: null,
            retryQueue: /* @__PURE__ */ new Set([a])
          }, e.updateQueue = t) : (e = t.retryQueue, e === null ? t.retryQueue = /* @__PURE__ */ new Set([a]) : e.add(a)), Ti(l, a, u)), !1;
      }
      throw Error(b(435, e.tag));
    }
    return Ti(l, a, u), yn(), !1;
  }
  if (Q)
    return t = Xl.current, t !== null ? (!(t.flags & 65536) && (t.flags |= 256), t.flags |= 65536, t.lanes = u, a !== Wi && (l = Error(b(422), { cause: a }), Ka(Jl(l, e)))) : (a !== Wi && (t = Error(b(423), {
      cause: a
    }), Ka(
      Jl(t, e)
    )), l = l.current.alternate, l.flags |= 65536, u &= -u, l.lanes |= u, a = Jl(a, e), u = sc(
      l.stateNode,
      a,
      u
    ), oi(l, u), el !== 4 && (el = 2)), !1;
  var n = Error(b(520), { cause: a });
  if (n = Jl(n, e), qa === null ? qa = [n] : qa.push(n), el !== 4 && (el = 2), t === null) return !0;
  a = Jl(a, e), e = t;
  do {
    switch (e.tag) {
      case 3:
        return e.flags |= 65536, l = u & -u, e.lanes |= l, l = sc(e.stateNode, a, l), oi(e, l), !1;
      case 1:
        if (t = e.type, n = e.stateNode, (e.flags & 128) === 0 && (typeof t.getDerivedStateFromError == "function" || n !== null && typeof n.componentDidCatch == "function" && (Lt === null || !Lt.has(n))))
          return e.flags |= 65536, u &= -u, e.lanes |= u, u = vd(u), gd(
            u,
            l,
            e,
            a
          ), oi(e, u), !1;
    }
    e = e.return;
  } while (e !== null);
  return !1;
}
var gf = Error(b(461)), fl = !1;
function vl(l, t, e, a) {
  t.child = l === null ? jr(t, null, e, a) : de(
    t,
    l.child,
    e,
    a
  );
}
function xs(l, t, e, a, u) {
  e = e.render;
  var n = t.ref;
  if ("ref" in a) {
    var i = {};
    for (var c in a)
      c !== "ref" && (i[c] = a[c]);
  } else i = a;
  return re(t), a = uf(
    l,
    t,
    e,
    i,
    n,
    u
  ), c = nf(), l !== null && !fl ? (cf(l, t, u), Tt(l, t, u)) : (Q && c && Wc(t), t.flags |= 1, vl(l, t, a, u), t.child);
}
function Ns(l, t, e, a, u) {
  if (l === null) {
    var n = e.type;
    return typeof n == "function" && !$c(n) && n.defaultProps === void 0 && e.compare === null ? (t.tag = 15, t.type = n, Sd(
      l,
      t,
      n,
      a,
      u
    )) : (l = qu(
      e.type,
      null,
      a,
      t,
      t.mode,
      u
    ), l.ref = t.ref, l.return = t, t.child = l);
  }
  if (n = l.child, !Sf(l, u)) {
    var i = n.memoizedProps;
    if (e = e.compare, e = e !== null ? e : wa, e(i, a) && l.ref === t.ref)
      return Tt(l, t, u);
  }
  return t.flags |= 1, l = gt(n, a), l.ref = t.ref, l.return = t, t.child = l;
}
function Sd(l, t, e, a, u) {
  if (l !== null) {
    var n = l.memoizedProps;
    if (wa(n, a) && l.ref === t.ref)
      if (fl = !1, t.pendingProps = a = n, Sf(l, u))
        l.flags & 131072 && (fl = !0);
      else
        return t.lanes = l.lanes, Tt(l, t, u);
  }
  return oc(
    l,
    t,
    e,
    a,
    u
  );
}
function bd(l, t, e, a) {
  var u = a.children, n = l !== null ? l.memoizedState : null;
  if (l === null && t.stateNode === null && (t.stateNode = {
    _visibility: 1,
    _pendingMarkers: null,
    _retryCache: null,
    _transitions: null
  }), a.mode === "hidden") {
    if (t.flags & 128) {
      if (n = n !== null ? n.baseLanes | e : e, l !== null) {
        for (a = t.child = l.child, u = 0; a !== null; )
          u = u | a.lanes | a.childLanes, a = a.sibling;
        a = u & ~n;
      } else a = 0, t.child = null;
      return js(
        l,
        t,
        n,
        e,
        a
      );
    }
    if (e & 536870912)
      t.memoizedState = { baseLanes: 0, cachePool: null }, l !== null && Gu(
        t,
        n !== null ? n.cachePool : null
      ), n !== null ? bs(t, n) : ac(), Hr(t);
    else
      return a = t.lanes = 536870912, js(
        l,
        t,
        n !== null ? n.baseLanes | e : e,
        e,
        a
      );
  } else
    n !== null ? (Gu(t, n.cachePool), bs(t, n), jt(), t.memoizedState = null) : (l !== null && Gu(t, null), ac(), jt());
  return vl(l, t, u, e), t.child;
}
function Aa(l, t) {
  return l !== null && l.tag === 22 || t.stateNode !== null || (t.stateNode = {
    _visibility: 1,
    _pendingMarkers: null,
    _retryCache: null,
    _transitions: null
  }), t.sibling;
}
function js(l, t, e, a, u) {
  var n = Pc();
  return n = n === null ? null : { parent: cl._currentValue, pool: n }, t.memoizedState = {
    baseLanes: e,
    cachePool: n
  }, l !== null && Gu(t, null), ac(), Hr(t), l !== null && ca(l, t, a, !0), t.childLanes = u, null;
}
function Lu(l, t) {
  return t = rn(
    { mode: t.mode, children: t.children },
    l.mode
  ), t.ref = l.ref, l.child = t, t.return = l, t;
}
function Us(l, t, e) {
  return de(t, l.child, null, e), l = Lu(t, t.pendingProps), l.flags |= 2, Cl(t), t.memoizedState = null, l;
}
function x0(l, t, e) {
  var a = t.pendingProps, u = (t.flags & 128) !== 0;
  if (t.flags &= -129, l === null) {
    if (Q) {
      if (a.mode === "hidden")
        return l = Lu(t, a), t.lanes = 536870912, Aa(null, l);
      if (uc(t), (l = F) ? (l = rm(
        l,
        kl
      ), l = l !== null && l.data === "&" ? l : null, l !== null && (t.memoizedState = {
        dehydrated: l,
        treeContext: Jt !== null ? { id: tt, overflow: et } : null,
        retryLane: 536870912,
        hydrationErrors: null
      }, e = zr(l), e.return = t, t.child = e, Sl = t, F = null)) : l = null, l === null) throw kt(t);
      return t.lanes = 536870912, null;
    }
    return Lu(t, a);
  }
  var n = l.memoizedState;
  if (n !== null) {
    var i = n.dehydrated;
    if (uc(t), u)
      if (t.flags & 256)
        t.flags &= -257, t = Us(
          l,
          t,
          e
        );
      else if (t.memoizedState !== null)
        t.child = l.child, t.flags |= 128, t = null;
      else throw Error(b(558));
    else if (fl || ca(l, t, e, !1), u = (e & l.childLanes) !== 0, fl || u) {
      if (a = k, a !== null && (i = Wo(a, e), i !== 0 && i !== n.retryLane))
        throw n.retryLane = i, Se(l, i), xl(a, l, i), gf;
      yn(), t = Us(
        l,
        t,
        e
      );
    } else
      l = n.treeContext, F = Wl(i.nextSibling), Sl = t, Q = !0, Gt = null, kl = !1, l !== null && Mr(t, l), t = Lu(t, a), t.flags |= 4096;
    return t;
  }
  return l = gt(l.child, {
    mode: a.mode,
    children: a.children
  }), l.ref = t.ref, t.child = l, l.return = t, l;
}
function Zu(l, t) {
  var e = t.ref;
  if (e === null)
    l !== null && l.ref !== null && (t.flags |= 4194816);
  else {
    if (typeof e != "function" && typeof e != "object")
      throw Error(b(284));
    (l === null || l.ref !== e) && (t.flags |= 4194816);
  }
}
function oc(l, t, e, a, u) {
  return re(t), e = uf(
    l,
    t,
    e,
    a,
    void 0,
    u
  ), a = nf(), l !== null && !fl ? (cf(l, t, u), Tt(l, t, u)) : (Q && a && Wc(t), t.flags |= 1, vl(l, t, e, u), t.child);
}
function Cs(l, t, e, a, u, n) {
  return re(t), t.updateQueue = null, e = Br(
    t,
    a,
    e,
    u
  ), Rr(l), a = nf(), l !== null && !fl ? (cf(l, t, n), Tt(l, t, n)) : (Q && a && Wc(t), t.flags |= 1, vl(l, t, e, n), t.child);
}
function Hs(l, t, e, a, u) {
  if (re(t), t.stateNode === null) {
    var n = Re, i = e.contextType;
    typeof i == "object" && i !== null && (n = bl(i)), n = new e(a, n), t.memoizedState = n.state !== null && n.state !== void 0 ? n.state : null, n.updater = fc, t.stateNode = n, n._reactInternals = t, n = t.stateNode, n.props = a, n.state = t.memoizedState, n.refs = {}, tf(t), i = e.contextType, n.context = typeof i == "object" && i !== null ? bl(i) : Re, n.state = t.memoizedState, i = e.getDerivedStateFromProps, typeof i == "function" && (di(
      t,
      e,
      i,
      a
    ), n.state = t.memoizedState), typeof e.getDerivedStateFromProps == "function" || typeof n.getSnapshotBeforeUpdate == "function" || typeof n.UNSAFE_componentWillMount != "function" && typeof n.componentWillMount != "function" || (i = n.state, typeof n.componentWillMount == "function" && n.componentWillMount(), typeof n.UNSAFE_componentWillMount == "function" && n.UNSAFE_componentWillMount(), i !== n.state && fc.enqueueReplaceState(n, n.state, null), Ha(t, a, n, u), Ca(), n.state = t.memoizedState), typeof n.componentDidMount == "function" && (t.flags |= 4194308), a = !0;
  } else if (l === null) {
    n = t.stateNode;
    var c = t.memoizedProps, f = he(e, c);
    n.props = f;
    var o = n.context, d = e.contextType;
    i = Re, typeof d == "object" && d !== null && (i = bl(d));
    var y = e.getDerivedStateFromProps;
    d = typeof y == "function" || typeof n.getSnapshotBeforeUpdate == "function", c = t.pendingProps !== c, d || typeof n.UNSAFE_componentWillReceiveProps != "function" && typeof n.componentWillReceiveProps != "function" || (c || o !== i) && Os(
      t,
      n,
      a,
      i
    ), Dt = !1;
    var h = t.memoizedState;
    n.state = h, Ha(t, a, n, u), Ca(), o = t.memoizedState, c || h !== o || Dt ? (typeof y == "function" && (di(
      t,
      e,
      y,
      a
    ), o = t.memoizedState), (f = Dt || Ms(
      t,
      e,
      f,
      a,
      h,
      o,
      i
    )) ? (d || typeof n.UNSAFE_componentWillMount != "function" && typeof n.componentWillMount != "function" || (typeof n.componentWillMount == "function" && n.componentWillMount(), typeof n.UNSAFE_componentWillMount == "function" && n.UNSAFE_componentWillMount()), typeof n.componentDidMount == "function" && (t.flags |= 4194308)) : (typeof n.componentDidMount == "function" && (t.flags |= 4194308), t.memoizedProps = a, t.memoizedState = o), n.props = a, n.state = o, n.context = i, a = f) : (typeof n.componentDidMount == "function" && (t.flags |= 4194308), a = !1);
  } else {
    n = t.stateNode, tc(l, t), i = t.memoizedProps, d = he(e, i), n.props = d, y = t.pendingProps, h = n.context, o = e.contextType, f = Re, typeof o == "object" && o !== null && (f = bl(o)), c = e.getDerivedStateFromProps, (o = typeof c == "function" || typeof n.getSnapshotBeforeUpdate == "function") || typeof n.UNSAFE_componentWillReceiveProps != "function" && typeof n.componentWillReceiveProps != "function" || (i !== y || h !== f) && Os(
      t,
      n,
      a,
      f
    ), Dt = !1, h = t.memoizedState, n.state = h, Ha(t, a, n, u), Ca();
    var v = t.memoizedState;
    i !== y || h !== v || Dt || l !== null && l.dependencies !== null && an(l.dependencies) ? (typeof c == "function" && (di(
      t,
      e,
      c,
      a
    ), v = t.memoizedState), (d = Dt || Ms(
      t,
      e,
      d,
      a,
      h,
      v,
      f
    ) || l !== null && l.dependencies !== null && an(l.dependencies)) ? (o || typeof n.UNSAFE_componentWillUpdate != "function" && typeof n.componentWillUpdate != "function" || (typeof n.componentWillUpdate == "function" && n.componentWillUpdate(a, v, f), typeof n.UNSAFE_componentWillUpdate == "function" && n.UNSAFE_componentWillUpdate(
      a,
      v,
      f
    )), typeof n.componentDidUpdate == "function" && (t.flags |= 4), typeof n.getSnapshotBeforeUpdate == "function" && (t.flags |= 1024)) : (typeof n.componentDidUpdate != "function" || i === l.memoizedProps && h === l.memoizedState || (t.flags |= 4), typeof n.getSnapshotBeforeUpdate != "function" || i === l.memoizedProps && h === l.memoizedState || (t.flags |= 1024), t.memoizedProps = a, t.memoizedState = v), n.props = a, n.state = v, n.context = f, a = d) : (typeof n.componentDidUpdate != "function" || i === l.memoizedProps && h === l.memoizedState || (t.flags |= 4), typeof n.getSnapshotBeforeUpdate != "function" || i === l.memoizedProps && h === l.memoizedState || (t.flags |= 1024), a = !1);
  }
  return n = a, Zu(l, t), a = (t.flags & 128) !== 0, n || a ? (n = t.stateNode, e = a && typeof e.getDerivedStateFromError != "function" ? null : n.render(), t.flags |= 1, l !== null && a ? (t.child = de(
    t,
    l.child,
    null,
    u
  ), t.child = de(
    t,
    null,
    e,
    u
  )) : vl(l, t, e, u), t.memoizedState = n.state, l = t.child) : l = Tt(
    l,
    t,
    u
  ), l;
}
function Rs(l, t, e, a) {
  return oe(), t.flags |= 256, vl(l, t, e, a), t.child;
}
var mi = {
  dehydrated: null,
  treeContext: null,
  retryLane: 0,
  hydrationErrors: null
};
function hi(l) {
  return { baseLanes: l, cachePool: Dr() };
}
function yi(l, t, e) {
  return l = l !== null ? l.childLanes & ~e : 0, t && (l |= Rl), l;
}
function _d(l, t, e) {
  var a = t.pendingProps, u = !1, n = (t.flags & 128) !== 0, i;
  if ((i = n) || (i = l !== null && l.memoizedState === null ? !1 : (al.current & 2) !== 0), i && (u = !0, t.flags &= -129), i = (t.flags & 32) !== 0, t.flags &= -33, l === null) {
    if (Q) {
      if (u ? Nt(t) : jt(), (l = F) ? (l = rm(
        l,
        kl
      ), l = l !== null && l.data !== "&" ? l : null, l !== null && (t.memoizedState = {
        dehydrated: l,
        treeContext: Jt !== null ? { id: tt, overflow: et } : null,
        retryLane: 536870912,
        hydrationErrors: null
      }, e = zr(l), e.return = t, t.child = e, Sl = t, F = null)) : l = null, l === null) throw kt(t);
      return zc(l) ? t.lanes = 32 : t.lanes = 536870912, null;
    }
    var c = a.children;
    return a = a.fallback, u ? (jt(), u = t.mode, c = rn(
      { mode: "hidden", children: c },
      u
    ), a = ie(
      a,
      u,
      e,
      null
    ), c.return = t, a.return = t, c.sibling = a, t.child = c, a = t.child, a.memoizedState = hi(e), a.childLanes = yi(
      l,
      i,
      e
    ), t.memoizedState = mi, Aa(null, a)) : (Nt(t), rc(t, c));
  }
  var f = l.memoizedState;
  if (f !== null && (c = f.dehydrated, c !== null)) {
    if (n)
      t.flags & 256 ? (Nt(t), t.flags &= -257, t = vi(
        l,
        t,
        e
      )) : t.memoizedState !== null ? (jt(), t.child = l.child, t.flags |= 128, t = null) : (jt(), c = a.fallback, u = t.mode, a = rn(
        { mode: "visible", children: a.children },
        u
      ), c = ie(
        c,
        u,
        e,
        null
      ), c.flags |= 2, a.return = t, c.return = t, a.sibling = c, t.child = a, de(
        t,
        l.child,
        null,
        e
      ), a = t.child, a.memoizedState = hi(e), a.childLanes = yi(
        l,
        i,
        e
      ), t.memoizedState = mi, t = Aa(null, a));
    else if (Nt(t), zc(c)) {
      if (i = c.nextSibling && c.nextSibling.dataset, i) var o = i.dgst;
      i = o, a = Error(b(419)), a.stack = "", a.digest = i, Ka({ value: a, source: null, stack: null }), t = vi(
        l,
        t,
        e
      );
    } else if (fl || ca(l, t, e, !1), i = (e & l.childLanes) !== 0, fl || i) {
      if (i = k, i !== null && (a = Wo(i, e), a !== 0 && a !== f.retryLane))
        throw f.retryLane = a, Se(l, a), xl(i, l, a), gf;
      Tc(c) || yn(), t = vi(
        l,
        t,
        e
      );
    } else
      Tc(c) ? (t.flags |= 192, t.child = l.child, t = null) : (l = f.treeContext, F = Wl(
        c.nextSibling
      ), Sl = t, Q = !0, Gt = null, kl = !1, l !== null && Mr(t, l), t = rc(
        t,
        a.children
      ), t.flags |= 4096);
    return t;
  }
  return u ? (jt(), c = a.fallback, u = t.mode, f = l.child, o = f.sibling, a = gt(f, {
    mode: "hidden",
    children: a.children
  }), a.subtreeFlags = f.subtreeFlags & 65011712, o !== null ? c = gt(
    o,
    c
  ) : (c = ie(
    c,
    u,
    e,
    null
  ), c.flags |= 2), c.return = t, a.return = t, a.sibling = c, t.child = a, Aa(null, a), a = t.child, c = l.child.memoizedState, c === null ? c = hi(e) : (u = c.cachePool, u !== null ? (f = cl._currentValue, u = u.parent !== f ? { parent: f, pool: f } : u) : u = Dr(), c = {
    baseLanes: c.baseLanes | e,
    cachePool: u
  }), a.memoizedState = c, a.childLanes = yi(
    l,
    i,
    e
  ), t.memoizedState = mi, Aa(l.child, a)) : (Nt(t), e = l.child, l = e.sibling, e = gt(e, {
    mode: "visible",
    children: a.children
  }), e.return = t, e.sibling = null, l !== null && (i = t.deletions, i === null ? (t.deletions = [l], t.flags |= 16) : i.push(l)), t.child = e, t.memoizedState = null, e);
}
function rc(l, t) {
  return t = rn(
    { mode: "visible", children: t },
    l.mode
  ), t.return = l, l.child = t;
}
function rn(l, t) {
  return l = Hl(22, l, null, t), l.lanes = 0, l;
}
function vi(l, t, e) {
  return de(t, l.child, null, e), l = rc(
    t,
    t.pendingProps.children
  ), l.flags |= 2, t.memoizedState = null, l;
}
function Bs(l, t, e) {
  l.lanes |= t;
  var a = l.alternate;
  a !== null && (a.lanes |= t), Ii(l.return, t, e);
}
function gi(l, t, e, a, u, n) {
  var i = l.memoizedState;
  i === null ? l.memoizedState = {
    isBackwards: t,
    rendering: null,
    renderingStartTime: 0,
    last: a,
    tail: e,
    tailMode: u,
    treeForkCount: n
  } : (i.isBackwards = t, i.rendering = null, i.renderingStartTime = 0, i.last = a, i.tail = e, i.tailMode = u, i.treeForkCount = n);
}
function Ed(l, t, e) {
  var a = t.pendingProps, u = a.revealOrder, n = a.tail;
  a = a.children;
  var i = al.current, c = (i & 2) !== 0;
  if (c ? (i = i & 1 | 2, t.flags |= 128) : i &= 1, $(al, i), vl(l, t, a, e), a = Q ? Va : 0, !c && l !== null && l.flags & 128)
    l: for (l = t.child; l !== null; ) {
      if (l.tag === 13)
        l.memoizedState !== null && Bs(l, e, t);
      else if (l.tag === 19)
        Bs(l, e, t);
      else if (l.child !== null) {
        l.child.return = l, l = l.child;
        continue;
      }
      if (l === t) break l;
      for (; l.sibling === null; ) {
        if (l.return === null || l.return === t)
          break l;
        l = l.return;
      }
      l.sibling.return = l.return, l = l.sibling;
    }
  switch (u) {
    case "forwards":
      for (e = t.child, u = null; e !== null; )
        l = e.alternate, l !== null && cn(l) === null && (u = e), e = e.sibling;
      e = u, e === null ? (u = t.child, t.child = null) : (u = e.sibling, e.sibling = null), gi(
        t,
        !1,
        u,
        e,
        n,
        a
      );
      break;
    case "backwards":
    case "unstable_legacy-backwards":
      for (e = null, u = t.child, t.child = null; u !== null; ) {
        if (l = u.alternate, l !== null && cn(l) === null) {
          t.child = u;
          break;
        }
        l = u.sibling, u.sibling = e, e = u, u = l;
      }
      gi(
        t,
        !0,
        e,
        null,
        n,
        a
      );
      break;
    case "together":
      gi(
        t,
        !1,
        null,
        null,
        void 0,
        a
      );
      break;
    default:
      t.memoizedState = null;
  }
  return t.child;
}
function Tt(l, t, e) {
  if (l !== null && (t.dependencies = l.dependencies), Wt |= t.lanes, !(e & t.childLanes))
    if (l !== null) {
      if (ca(
        l,
        t,
        e,
        !1
      ), (e & t.childLanes) === 0)
        return null;
    } else return null;
  if (l !== null && t.child !== l.child)
    throw Error(b(153));
  if (t.child !== null) {
    for (l = t.child, e = gt(l, l.pendingProps), t.child = e, e.return = t; l.sibling !== null; )
      l = l.sibling, e = e.sibling = gt(l, l.pendingProps), e.return = t;
    e.sibling = null;
  }
  return t.child;
}
function Sf(l, t) {
  return l.lanes & t ? !0 : (l = l.dependencies, !!(l !== null && an(l)));
}
function N0(l, t, e) {
  switch (t.tag) {
    case 3:
      Wu(t, t.stateNode.containerInfo), xt(t, cl, l.memoizedState.cache), oe();
      break;
    case 27:
    case 5:
      Gi(t);
      break;
    case 4:
      Wu(t, t.stateNode.containerInfo);
      break;
    case 10:
      xt(
        t,
        t.type,
        t.memoizedProps.value
      );
      break;
    case 31:
      if (t.memoizedState !== null)
        return t.flags |= 128, uc(t), null;
      break;
    case 13:
      var a = t.memoizedState;
      if (a !== null)
        return a.dehydrated !== null ? (Nt(t), t.flags |= 128, null) : e & t.child.childLanes ? _d(l, t, e) : (Nt(t), l = Tt(
          l,
          t,
          e
        ), l !== null ? l.sibling : null);
      Nt(t);
      break;
    case 19:
      var u = (l.flags & 128) !== 0;
      if (a = (e & t.childLanes) !== 0, a || (ca(
        l,
        t,
        e,
        !1
      ), a = (e & t.childLanes) !== 0), u) {
        if (a)
          return Ed(
            l,
            t,
            e
          );
        t.flags |= 128;
      }
      if (u = t.memoizedState, u !== null && (u.rendering = null, u.tail = null, u.lastEffect = null), $(al, al.current), a) break;
      return null;
    case 22:
      return t.lanes = 0, bd(
        l,
        t,
        e,
        t.pendingProps
      );
    case 24:
      xt(t, cl, l.memoizedState.cache);
  }
  return Tt(l, t, e);
}
function pd(l, t, e) {
  if (l !== null)
    if (l.memoizedProps !== t.pendingProps)
      fl = !0;
    else {
      if (!Sf(l, e) && !(t.flags & 128))
        return fl = !1, N0(
          l,
          t,
          e
        );
      fl = !!(l.flags & 131072);
    }
  else
    fl = !1, Q && t.flags & 1048576 && Ar(t, Va, t.index);
  switch (t.lanes = 0, t.tag) {
    case 16:
      l: {
        var a = t.pendingProps;
        if (l = ae(t.elementType), t.type = l, typeof l == "function")
          $c(l) ? (a = he(l, a), t.tag = 1, t = Hs(
            null,
            t,
            l,
            a,
            e
          )) : (t.tag = 0, t = oc(
            null,
            t,
            l,
            a,
            e
          ));
        else {
          if (l != null) {
            var u = l.$$typeof;
            if (u === Rc) {
              t.tag = 11, t = xs(
                null,
                t,
                l,
                a,
                e
              );
              break l;
            } else if (u === Bc) {
              t.tag = 14, t = Ns(
                null,
                t,
                l,
                a,
                e
              );
              break l;
            }
          }
          throw t = Yi(l) || l, Error(b(306, t, ""));
        }
      }
      return t;
    case 0:
      return oc(
        l,
        t,
        t.type,
        t.pendingProps,
        e
      );
    case 1:
      return a = t.type, u = he(
        a,
        t.pendingProps
      ), Hs(
        l,
        t,
        a,
        u,
        e
      );
    case 3:
      l: {
        if (Wu(
          t,
          t.stateNode.containerInfo
        ), l === null) throw Error(b(387));
        a = t.pendingProps;
        var n = t.memoizedState;
        u = n.element, tc(l, t), Ha(t, a, null, e);
        var i = t.memoizedState;
        if (a = i.cache, xt(t, cl, a), a !== n.cache && Pi(
          t,
          [cl],
          e,
          !0
        ), Ca(), a = i.element, n.isDehydrated)
          if (n = {
            element: a,
            isDehydrated: !1,
            cache: i.cache
          }, t.updateQueue.baseState = n, t.memoizedState = n, t.flags & 256) {
            t = Rs(
              l,
              t,
              a,
              e
            );
            break l;
          } else if (a !== u) {
            u = Jl(
              Error(b(424)),
              t
            ), Ka(u), t = Rs(
              l,
              t,
              a,
              e
            );
            break l;
          } else {
            switch (l = t.stateNode.containerInfo, l.nodeType) {
              case 9:
                l = l.body;
                break;
              default:
                l = l.nodeName === "HTML" ? l.ownerDocument.body : l;
            }
            for (F = Wl(l.firstChild), Sl = t, Q = !0, Gt = null, kl = !0, e = jr(
              t,
              null,
              a,
              e
            ), t.child = e; e; )
              e.flags = e.flags & -3 | 4096, e = e.sibling;
          }
        else {
          if (oe(), a === u) {
            t = Tt(
              l,
              t,
              e
            );
            break l;
          }
          vl(l, t, a, e);
        }
        t = t.child;
      }
      return t;
    case 26:
      return Zu(l, t), l === null ? (e = eo(
        t.type,
        null,
        t.pendingProps,
        null
      )) ? t.memoizedState = e : Q || (e = t.type, l = t.pendingProps, a = bn(
        qt.current
      ).createElement(e), a[gl] = t, a[Nl] = l, _l(a, e, l), hl(a), t.stateNode = a) : t.memoizedState = eo(
        t.type,
        l.memoizedProps,
        t.pendingProps,
        l.memoizedState
      ), null;
    case 27:
      return Gi(t), l === null && Q && (a = t.stateNode = dm(
        t.type,
        t.pendingProps,
        qt.current
      ), Sl = t, kl = !0, u = F, It(t.type) ? (Ac = u, F = Wl(a.firstChild)) : F = u), vl(
        l,
        t,
        t.pendingProps.children,
        e
      ), Zu(l, t), l === null && (t.flags |= 4194304), t.child;
    case 5:
      return l === null && Q && ((u = a = F) && (a = iy(
        a,
        t.type,
        t.pendingProps,
        kl
      ), a !== null ? (t.stateNode = a, Sl = t, F = Wl(a.firstChild), kl = !1, u = !0) : u = !1), u || kt(t)), Gi(t), u = t.type, n = t.pendingProps, i = l !== null ? l.memoizedProps : null, a = n.children, Ec(u, n) ? a = null : i !== null && Ec(u, i) && (t.flags |= 32), t.memoizedState !== null && (u = uf(
        l,
        t,
        E0,
        null,
        null,
        e
      ), Ia._currentValue = u), Zu(l, t), vl(l, t, a, e), t.child;
    case 6:
      return l === null && Q && ((l = e = F) && (e = cy(
        e,
        t.pendingProps,
        kl
      ), e !== null ? (t.stateNode = e, Sl = t, F = null, l = !0) : l = !1), l || kt(t)), null;
    case 13:
      return _d(l, t, e);
    case 4:
      return Wu(
        t,
        t.stateNode.containerInfo
      ), a = t.pendingProps, l === null ? t.child = de(
        t,
        null,
        a,
        e
      ) : vl(l, t, a, e), t.child;
    case 11:
      return xs(
        l,
        t,
        t.type,
        t.pendingProps,
        e
      );
    case 7:
      return vl(
        l,
        t,
        t.pendingProps,
        e
      ), t.child;
    case 8:
      return vl(
        l,
        t,
        t.pendingProps.children,
        e
      ), t.child;
    case 12:
      return vl(
        l,
        t,
        t.pendingProps.children,
        e
      ), t.child;
    case 10:
      return a = t.pendingProps, xt(t, t.type, a.value), vl(l, t, a.children, e), t.child;
    case 9:
      return u = t.type._context, a = t.pendingProps.children, re(t), u = bl(u), a = a(u), t.flags |= 1, vl(l, t, a, e), t.child;
    case 14:
      return Ns(
        l,
        t,
        t.type,
        t.pendingProps,
        e
      );
    case 15:
      return Sd(
        l,
        t,
        t.type,
        t.pendingProps,
        e
      );
    case 19:
      return Ed(l, t, e);
    case 31:
      return x0(l, t, e);
    case 22:
      return bd(
        l,
        t,
        e,
        t.pendingProps
      );
    case 24:
      return re(t), a = bl(cl), l === null ? (u = Pc(), u === null && (u = k, n = Ic(), u.pooledCache = n, n.refCount++, n !== null && (u.pooledCacheLanes |= e), u = n), t.memoizedState = { parent: a, cache: u }, tf(t), xt(t, cl, u)) : (l.lanes & e && (tc(l, t), Ha(t, null, null, e), Ca()), u = l.memoizedState, n = t.memoizedState, u.parent !== a ? (u = { parent: a, cache: a }, t.memoizedState = u, t.lanes === 0 && (t.memoizedState = t.updateQueue.baseState = u), xt(t, cl, a)) : (a = n.cache, xt(t, cl, a), a !== u.cache && Pi(
        t,
        [cl],
        e,
        !0
      ))), vl(
        l,
        t,
        t.pendingProps.children,
        e
      ), t.child;
    case 29:
      throw t.pendingProps;
  }
  throw Error(b(156, t.tag));
}
function ft(l) {
  l.flags |= 4;
}
function Si(l, t, e, a, u) {
  if ((t = (l.mode & 32) !== 0) && (t = !1), t) {
    if (l.flags |= 16777216, (u & 335544128) === u)
      if (l.stateNode.complete) l.flags |= 8192;
      else if (Kd()) l.flags |= 8192;
      else
        throw fe = un, lf;
  } else l.flags &= -16777217;
}
function Ys(l, t) {
  if (t.type !== "stylesheet" || t.state.loading & 4)
    l.flags &= -16777217;
  else if (l.flags |= 16777216, !ym(t))
    if (Kd()) l.flags |= 8192;
    else
      throw fe = un, lf;
}
function Du(l, t) {
  t !== null && (l.flags |= 4), l.flags & 16384 && (t = l.tag !== 22 ? Jo() : 536870912, l.lanes |= t, Ie |= t);
}
function Sa(l, t) {
  if (!Q)
    switch (l.tailMode) {
      case "hidden":
        t = l.tail;
        for (var e = null; t !== null; )
          t.alternate !== null && (e = t), t = t.sibling;
        e === null ? l.tail = null : e.sibling = null;
        break;
      case "collapsed":
        e = l.tail;
        for (var a = null; e !== null; )
          e.alternate !== null && (a = e), e = e.sibling;
        a === null ? t || l.tail === null ? l.tail = null : l.tail.sibling = null : a.sibling = null;
    }
}
function W(l) {
  var t = l.alternate !== null && l.alternate.child === l.child, e = 0, a = 0;
  if (t)
    for (var u = l.child; u !== null; )
      e |= u.lanes | u.childLanes, a |= u.subtreeFlags & 65011712, a |= u.flags & 65011712, u.return = l, u = u.sibling;
  else
    for (u = l.child; u !== null; )
      e |= u.lanes | u.childLanes, a |= u.subtreeFlags, a |= u.flags, u.return = l, u = u.sibling;
  return l.subtreeFlags |= a, l.childLanes = e, t;
}
function j0(l, t, e) {
  var a = t.pendingProps;
  switch (Fc(t), t.tag) {
    case 16:
    case 15:
    case 0:
    case 11:
    case 7:
    case 8:
    case 12:
    case 9:
    case 14:
      return W(t), null;
    case 1:
      return W(t), null;
    case 3:
      return e = t.stateNode, a = null, l !== null && (a = l.memoizedState.cache), t.memoizedState.cache !== a && (t.flags |= 2048), St(cl), Ke(), e.pendingContext && (e.context = e.pendingContext, e.pendingContext = null), (l === null || l.child === null) && (Ee(t) ? ft(t) : l === null || l.memoizedState.isDehydrated && !(t.flags & 256) || (t.flags |= 1024, si())), W(t), null;
    case 26:
      var u = t.type, n = t.memoizedState;
      return l === null ? (ft(t), n !== null ? (W(t), Ys(t, n)) : (W(t), Si(
        t,
        u,
        null,
        a,
        e
      ))) : n ? n !== l.memoizedState ? (ft(t), W(t), Ys(t, n)) : (W(t), t.flags &= -16777217) : (l = l.memoizedProps, l !== a && ft(t), W(t), Si(
        t,
        u,
        l,
        a,
        e
      )), null;
    case 27:
      if (Fu(t), e = qt.current, u = t.type, l !== null && t.stateNode != null)
        l.memoizedProps !== a && ft(t);
      else {
        if (!a) {
          if (t.stateNode === null)
            throw Error(b(166));
          return W(t), null;
        }
        l = ut.current, Ee(t) ? ds(t) : (l = dm(u, a, e), t.stateNode = l, ft(t));
      }
      return W(t), null;
    case 5:
      if (Fu(t), u = t.type, l !== null && t.stateNode != null)
        l.memoizedProps !== a && ft(t);
      else {
        if (!a) {
          if (t.stateNode === null)
            throw Error(b(166));
          return W(t), null;
        }
        if (n = ut.current, Ee(t))
          ds(t);
        else {
          var i = bn(
            qt.current
          );
          switch (n) {
            case 1:
              n = i.createElementNS(
                "http://www.w3.org/2000/svg",
                u
              );
              break;
            case 2:
              n = i.createElementNS(
                "http://www.w3.org/1998/Math/MathML",
                u
              );
              break;
            default:
              switch (u) {
                case "svg":
                  n = i.createElementNS(
                    "http://www.w3.org/2000/svg",
                    u
                  );
                  break;
                case "math":
                  n = i.createElementNS(
                    "http://www.w3.org/1998/Math/MathML",
                    u
                  );
                  break;
                case "script":
                  n = i.createElement("div"), n.innerHTML = "<script><\/script>", n = n.removeChild(
                    n.firstChild
                  );
                  break;
                case "select":
                  n = typeof a.is == "string" ? i.createElement("select", {
                    is: a.is
                  }) : i.createElement("select"), a.multiple ? n.multiple = !0 : a.size && (n.size = a.size);
                  break;
                default:
                  n = typeof a.is == "string" ? i.createElement(u, { is: a.is }) : i.createElement(u);
              }
          }
          n[gl] = t, n[Nl] = a;
          l: for (i = t.child; i !== null; ) {
            if (i.tag === 5 || i.tag === 6)
              n.appendChild(i.stateNode);
            else if (i.tag !== 4 && i.tag !== 27 && i.child !== null) {
              i.child.return = i, i = i.child;
              continue;
            }
            if (i === t) break l;
            for (; i.sibling === null; ) {
              if (i.return === null || i.return === t)
                break l;
              i = i.return;
            }
            i.sibling.return = i.return, i = i.sibling;
          }
          t.stateNode = n;
          l: switch (_l(n, u, a), u) {
            case "button":
            case "input":
            case "select":
            case "textarea":
              a = !!a.autoFocus;
              break l;
            case "img":
              a = !0;
              break l;
            default:
              a = !1;
          }
          a && ft(t);
        }
      }
      return W(t), Si(
        t,
        t.type,
        l === null ? null : l.memoizedProps,
        t.pendingProps,
        e
      ), null;
    case 6:
      if (l && t.stateNode != null)
        l.memoizedProps !== a && ft(t);
      else {
        if (typeof a != "string" && t.stateNode === null)
          throw Error(b(166));
        if (l = qt.current, Ee(t)) {
          if (l = t.stateNode, e = t.memoizedProps, a = null, u = Sl, u !== null)
            switch (u.tag) {
              case 27:
              case 5:
                a = u.memoizedProps;
            }
          l[gl] = t, l = !!(l.nodeValue === e || a !== null && a.suppressHydrationWarning === !0 || fm(l.nodeValue, e)), l || kt(t, !0);
        } else
          l = bn(l).createTextNode(
            a
          ), l[gl] = t, t.stateNode = l;
      }
      return W(t), null;
    case 31:
      if (e = t.memoizedState, l === null || l.memoizedState !== null) {
        if (a = Ee(t), e !== null) {
          if (l === null) {
            if (!a) throw Error(b(318));
            if (l = t.memoizedState, l = l !== null ? l.dehydrated : null, !l) throw Error(b(557));
            l[gl] = t;
          } else
            oe(), !(t.flags & 128) && (t.memoizedState = null), t.flags |= 4;
          W(t), l = !1;
        } else
          e = si(), l !== null && l.memoizedState !== null && (l.memoizedState.hydrationErrors = e), l = !0;
        if (!l)
          return t.flags & 256 ? (Cl(t), t) : (Cl(t), null);
        if (t.flags & 128)
          throw Error(b(558));
      }
      return W(t), null;
    case 13:
      if (a = t.memoizedState, l === null || l.memoizedState !== null && l.memoizedState.dehydrated !== null) {
        if (u = Ee(t), a !== null && a.dehydrated !== null) {
          if (l === null) {
            if (!u) throw Error(b(318));
            if (u = t.memoizedState, u = u !== null ? u.dehydrated : null, !u) throw Error(b(317));
            u[gl] = t;
          } else
            oe(), !(t.flags & 128) && (t.memoizedState = null), t.flags |= 4;
          W(t), u = !1;
        } else
          u = si(), l !== null && l.memoizedState !== null && (l.memoizedState.hydrationErrors = u), u = !0;
        if (!u)
          return t.flags & 256 ? (Cl(t), t) : (Cl(t), null);
      }
      return Cl(t), t.flags & 128 ? (t.lanes = e, t) : (e = a !== null, l = l !== null && l.memoizedState !== null, e && (a = t.child, u = null, a.alternate !== null && a.alternate.memoizedState !== null && a.alternate.memoizedState.cachePool !== null && (u = a.alternate.memoizedState.cachePool.pool), n = null, a.memoizedState !== null && a.memoizedState.cachePool !== null && (n = a.memoizedState.cachePool.pool), n !== u && (a.flags |= 2048)), e !== l && e && (t.child.flags |= 8192), Du(t, t.updateQueue), W(t), null);
    case 4:
      return Ke(), l === null && Af(t.stateNode.containerInfo), W(t), null;
    case 10:
      return St(t.type), W(t), null;
    case 19:
      if (yl(al), a = t.memoizedState, a === null) return W(t), null;
      if (u = (t.flags & 128) !== 0, n = a.rendering, n === null)
        if (u) Sa(a, !1);
        else {
          if (el !== 0 || l !== null && l.flags & 128)
            for (l = t.child; l !== null; ) {
              if (n = cn(l), n !== null) {
                for (t.flags |= 128, Sa(a, !1), l = n.updateQueue, t.updateQueue = l, Du(t, l), t.subtreeFlags = 0, l = e, e = t.child; e !== null; )
                  Tr(e, l), e = e.sibling;
                return $(
                  al,
                  al.current & 1 | 2
                ), Q && dt(t, a.treeForkCount), t.child;
              }
              l = l.sibling;
            }
          a.tail !== null && Bl() > mn && (t.flags |= 128, u = !0, Sa(a, !1), t.lanes = 4194304);
        }
      else {
        if (!u)
          if (l = cn(n), l !== null) {
            if (t.flags |= 128, u = !0, l = l.updateQueue, t.updateQueue = l, Du(t, l), Sa(a, !0), a.tail === null && a.tailMode === "hidden" && !n.alternate && !Q)
              return W(t), null;
          } else
            2 * Bl() - a.renderingStartTime > mn && e !== 536870912 && (t.flags |= 128, u = !0, Sa(a, !1), t.lanes = 4194304);
        a.isBackwards ? (n.sibling = t.child, t.child = n) : (l = a.last, l !== null ? l.sibling = n : t.child = n, a.last = n);
      }
      return a.tail !== null ? (l = a.tail, a.rendering = l, a.tail = l.sibling, a.renderingStartTime = Bl(), l.sibling = null, e = al.current, $(
        al,
        u ? e & 1 | 2 : e & 1
      ), Q && dt(t, a.treeForkCount), l) : (W(t), null);
    case 22:
    case 23:
      return Cl(t), ef(), a = t.memoizedState !== null, l !== null ? l.memoizedState !== null !== a && (t.flags |= 8192) : a && (t.flags |= 8192), a ? e & 536870912 && !(t.flags & 128) && (W(t), t.subtreeFlags & 6 && (t.flags |= 8192)) : W(t), e = t.updateQueue, e !== null && Du(t, e.retryQueue), e = null, l !== null && l.memoizedState !== null && l.memoizedState.cachePool !== null && (e = l.memoizedState.cachePool.pool), a = null, t.memoizedState !== null && t.memoizedState.cachePool !== null && (a = t.memoizedState.cachePool.pool), a !== e && (t.flags |= 2048), l !== null && yl(ce), null;
    case 24:
      return e = null, l !== null && (e = l.memoizedState.cache), t.memoizedState.cache !== e && (t.flags |= 2048), St(cl), W(t), null;
    case 25:
      return null;
    case 30:
      return null;
  }
  throw Error(b(156, t.tag));
}
function U0(l, t) {
  switch (Fc(t), t.tag) {
    case 1:
      return l = t.flags, l & 65536 ? (t.flags = l & -65537 | 128, t) : null;
    case 3:
      return St(cl), Ke(), l = t.flags, l & 65536 && !(l & 128) ? (t.flags = l & -65537 | 128, t) : null;
    case 26:
    case 27:
    case 5:
      return Fu(t), null;
    case 31:
      if (t.memoizedState !== null) {
        if (Cl(t), t.alternate === null)
          throw Error(b(340));
        oe();
      }
      return l = t.flags, l & 65536 ? (t.flags = l & -65537 | 128, t) : null;
    case 13:
      if (Cl(t), l = t.memoizedState, l !== null && l.dehydrated !== null) {
        if (t.alternate === null)
          throw Error(b(340));
        oe();
      }
      return l = t.flags, l & 65536 ? (t.flags = l & -65537 | 128, t) : null;
    case 19:
      return yl(al), null;
    case 4:
      return Ke(), null;
    case 10:
      return St(t.type), null;
    case 22:
    case 23:
      return Cl(t), ef(), l !== null && yl(ce), l = t.flags, l & 65536 ? (t.flags = l & -65537 | 128, t) : null;
    case 24:
      return St(cl), null;
    case 25:
      return null;
    default:
      return null;
  }
}
function Td(l, t) {
  switch (Fc(t), t.tag) {
    case 3:
      St(cl), Ke();
      break;
    case 26:
    case 27:
    case 5:
      Fu(t);
      break;
    case 4:
      Ke();
      break;
    case 31:
      t.memoizedState !== null && Cl(t);
      break;
    case 13:
      Cl(t);
      break;
    case 19:
      yl(al);
      break;
    case 10:
      St(t.type);
      break;
    case 22:
    case 23:
      Cl(t), ef(), l !== null && yl(ce);
      break;
    case 24:
      St(cl);
  }
}
function su(l, t) {
  try {
    var e = t.updateQueue, a = e !== null ? e.lastEffect : null;
    if (a !== null) {
      var u = a.next;
      e = u;
      do {
        if ((e.tag & l) === l) {
          a = void 0;
          var n = e.create, i = e.inst;
          a = n(), i.destroy = a;
        }
        e = e.next;
      } while (e !== u);
    }
  } catch (c) {
    V(t, t.return, c);
  }
}
function $t(l, t, e) {
  try {
    var a = t.updateQueue, u = a !== null ? a.lastEffect : null;
    if (u !== null) {
      var n = u.next;
      a = n;
      do {
        if ((a.tag & l) === l) {
          var i = a.inst, c = i.destroy;
          if (c !== void 0) {
            i.destroy = void 0, u = t;
            var f = e, o = c;
            try {
              o();
            } catch (d) {
              V(
                u,
                f,
                d
              );
            }
          }
        }
        a = a.next;
      } while (a !== n);
    }
  } catch (d) {
    V(t, t.return, d);
  }
}
function zd(l) {
  var t = l.updateQueue;
  if (t !== null) {
    var e = l.stateNode;
    try {
      Cr(t, e);
    } catch (a) {
      V(l, l.return, a);
    }
  }
}
function Ad(l, t, e) {
  e.props = he(
    l.type,
    l.memoizedProps
  ), e.state = l.memoizedState;
  try {
    e.componentWillUnmount();
  } catch (a) {
    V(l, t, a);
  }
}
function Ba(l, t) {
  try {
    var e = l.ref;
    if (e !== null) {
      switch (l.tag) {
        case 26:
        case 27:
        case 5:
          var a = l.stateNode;
          break;
        case 30:
          a = l.stateNode;
          break;
        default:
          a = l.stateNode;
      }
      typeof e == "function" ? l.refCleanup = e(a) : e.current = a;
    }
  } catch (u) {
    V(l, t, u);
  }
}
function at(l, t) {
  var e = l.ref, a = l.refCleanup;
  if (e !== null)
    if (typeof a == "function")
      try {
        a();
      } catch (u) {
        V(l, t, u);
      } finally {
        l.refCleanup = null, l = l.alternate, l != null && (l.refCleanup = null);
      }
    else if (typeof e == "function")
      try {
        e(null);
      } catch (u) {
        V(l, t, u);
      }
    else e.current = null;
}
function Md(l) {
  var t = l.type, e = l.memoizedProps, a = l.stateNode;
  try {
    l: switch (t) {
      case "button":
      case "input":
      case "select":
      case "textarea":
        e.autoFocus && a.focus();
        break l;
      case "img":
        e.src ? a.src = e.src : e.srcSet && (a.srcset = e.srcSet);
    }
  } catch (u) {
    V(l, l.return, u);
  }
}
function bi(l, t, e) {
  try {
    var a = l.stateNode;
    ly(a, l.type, e, t), a[Nl] = t;
  } catch (u) {
    V(l, l.return, u);
  }
}
function Od(l) {
  return l.tag === 5 || l.tag === 3 || l.tag === 26 || l.tag === 27 && It(l.type) || l.tag === 4;
}
function _i(l) {
  l: for (; ; ) {
    for (; l.sibling === null; ) {
      if (l.return === null || Od(l.return)) return null;
      l = l.return;
    }
    for (l.sibling.return = l.return, l = l.sibling; l.tag !== 5 && l.tag !== 6 && l.tag !== 18; ) {
      if (l.tag === 27 && It(l.type) || l.flags & 2 || l.child === null || l.tag === 4) continue l;
      l.child.return = l, l = l.child;
    }
    if (!(l.flags & 2)) return l.stateNode;
  }
}
function dc(l, t, e) {
  var a = l.tag;
  if (a === 5 || a === 6)
    l = l.stateNode, t ? (e.nodeType === 9 ? e.body : e.nodeName === "HTML" ? e.ownerDocument.body : e).insertBefore(l, t) : (t = e.nodeType === 9 ? e.body : e.nodeName === "HTML" ? e.ownerDocument.body : e, t.appendChild(l), e = e._reactRootContainer, e != null || t.onclick !== null || (t.onclick = yt));
  else if (a !== 4 && (a === 27 && It(l.type) && (e = l.stateNode, t = null), l = l.child, l !== null))
    for (dc(l, t, e), l = l.sibling; l !== null; )
      dc(l, t, e), l = l.sibling;
}
function dn(l, t, e) {
  var a = l.tag;
  if (a === 5 || a === 6)
    l = l.stateNode, t ? e.insertBefore(l, t) : e.appendChild(l);
  else if (a !== 4 && (a === 27 && It(l.type) && (e = l.stateNode), l = l.child, l !== null))
    for (dn(l, t, e), l = l.sibling; l !== null; )
      dn(l, t, e), l = l.sibling;
}
function Dd(l) {
  var t = l.stateNode, e = l.memoizedProps;
  try {
    for (var a = l.type, u = t.attributes; u.length; )
      t.removeAttributeNode(u[0]);
    _l(t, a, e), t[gl] = l, t[Nl] = e;
  } catch (n) {
    V(l, l.return, n);
  }
}
var mt = !1, il = !1, Ei = !1, qs = typeof WeakSet == "function" ? WeakSet : Set, ml = null;
function C0(l, t) {
  if (l = l.containerInfo, bc = Tn, l = yr(l), Kc(l)) {
    if ("selectionStart" in l)
      var e = {
        start: l.selectionStart,
        end: l.selectionEnd
      };
    else
      l: {
        e = (e = l.ownerDocument) && e.defaultView || window;
        var a = e.getSelection && e.getSelection();
        if (a && a.rangeCount !== 0) {
          e = a.anchorNode;
          var u = a.anchorOffset, n = a.focusNode;
          a = a.focusOffset;
          try {
            e.nodeType, n.nodeType;
          } catch {
            e = null;
            break l;
          }
          var i = 0, c = -1, f = -1, o = 0, d = 0, y = l, h = null;
          t: for (; ; ) {
            for (var v; y !== e || u !== 0 && y.nodeType !== 3 || (c = i + u), y !== n || a !== 0 && y.nodeType !== 3 || (f = i + a), y.nodeType === 3 && (i += y.nodeValue.length), (v = y.firstChild) !== null; )
              h = y, y = v;
            for (; ; ) {
              if (y === l) break t;
              if (h === e && ++o === u && (c = i), h === n && ++d === a && (f = i), (v = y.nextSibling) !== null) break;
              y = h, h = y.parentNode;
            }
            y = v;
          }
          e = c === -1 || f === -1 ? null : { start: c, end: f };
        } else e = null;
      }
    e = e || { start: 0, end: 0 };
  } else e = null;
  for (_c = { focusedElem: l, selectionRange: e }, Tn = !1, ml = t; ml !== null; )
    if (t = ml, l = t.child, (t.subtreeFlags & 1028) !== 0 && l !== null)
      l.return = t, ml = l;
    else
      for (; ml !== null; ) {
        switch (t = ml, n = t.alternate, l = t.flags, t.tag) {
          case 0:
            if (l & 4 && (l = t.updateQueue, l = l !== null ? l.events : null, l !== null))
              for (e = 0; e < l.length; e++)
                u = l[e], u.ref.impl = u.nextImpl;
            break;
          case 11:
          case 15:
            break;
          case 1:
            if (l & 1024 && n !== null) {
              l = void 0, e = t, u = n.memoizedProps, n = n.memoizedState, a = e.stateNode;
              try {
                var p = he(
                  e.type,
                  u
                );
                l = a.getSnapshotBeforeUpdate(
                  p,
                  n
                ), a.__reactInternalSnapshotBeforeUpdate = l;
              } catch (T) {
                V(
                  e,
                  e.return,
                  T
                );
              }
            }
            break;
          case 3:
            if (l & 1024) {
              if (l = t.stateNode.containerInfo, e = l.nodeType, e === 9)
                pc(l);
              else if (e === 1)
                switch (l.nodeName) {
                  case "HEAD":
                  case "HTML":
                  case "BODY":
                    pc(l);
                    break;
                  default:
                    l.textContent = "";
                }
            }
            break;
          case 5:
          case 26:
          case 27:
          case 6:
          case 4:
          case 17:
            break;
          default:
            if (l & 1024) throw Error(b(163));
        }
        if (l = t.sibling, l !== null) {
          l.return = t.return, ml = l;
          break;
        }
        ml = t.return;
      }
}
function xd(l, t, e) {
  var a = e.flags;
  switch (e.tag) {
    case 0:
    case 11:
    case 15:
      ot(l, e), a & 4 && su(5, e);
      break;
    case 1:
      if (ot(l, e), a & 4)
        if (l = e.stateNode, t === null)
          try {
            l.componentDidMount();
          } catch (i) {
            V(e, e.return, i);
          }
        else {
          var u = he(
            e.type,
            t.memoizedProps
          );
          t = t.memoizedState;
          try {
            l.componentDidUpdate(
              u,
              t,
              l.__reactInternalSnapshotBeforeUpdate
            );
          } catch (i) {
            V(
              e,
              e.return,
              i
            );
          }
        }
      a & 64 && zd(e), a & 512 && Ba(e, e.return);
      break;
    case 3:
      if (ot(l, e), a & 64 && (l = e.updateQueue, l !== null)) {
        if (t = null, e.child !== null)
          switch (e.child.tag) {
            case 27:
            case 5:
              t = e.child.stateNode;
              break;
            case 1:
              t = e.child.stateNode;
          }
        try {
          Cr(l, t);
        } catch (i) {
          V(e, e.return, i);
        }
      }
      break;
    case 27:
      t === null && a & 4 && Dd(e);
    case 26:
    case 5:
      ot(l, e), t === null && a & 4 && Md(e), a & 512 && Ba(e, e.return);
      break;
    case 12:
      ot(l, e);
      break;
    case 31:
      ot(l, e), a & 4 && Ud(l, e);
      break;
    case 13:
      ot(l, e), a & 4 && Cd(l, e), a & 64 && (l = e.memoizedState, l !== null && (l = l.dehydrated, l !== null && (e = L0.bind(
        null,
        e
      ), fy(l, e))));
      break;
    case 22:
      if (a = e.memoizedState !== null || mt, !a) {
        t = t !== null && t.memoizedState !== null || il, u = mt;
        var n = il;
        mt = a, (il = t) && !n ? rt(
          l,
          e,
          (e.subtreeFlags & 8772) !== 0
        ) : ot(l, e), mt = u, il = n;
      }
      break;
    case 30:
      break;
    default:
      ot(l, e);
  }
}
function Nd(l) {
  var t = l.alternate;
  t !== null && (l.alternate = null, Nd(t)), l.child = null, l.deletions = null, l.sibling = null, l.tag === 5 && (t = l.stateNode, t !== null && Qc(t)), l.stateNode = null, l.return = null, l.dependencies = null, l.memoizedProps = null, l.memoizedState = null, l.pendingProps = null, l.stateNode = null, l.updateQueue = null;
}
var ll = null, Ol = !1;
function st(l, t, e) {
  for (e = e.child; e !== null; )
    jd(l, t, e), e = e.sibling;
}
function jd(l, t, e) {
  if (Yl && typeof Yl.onCommitFiberUnmount == "function")
    try {
      Yl.onCommitFiberUnmount(eu, e);
    } catch {
    }
  switch (e.tag) {
    case 26:
      il || at(e, t), st(
        l,
        t,
        e
      ), e.memoizedState ? e.memoizedState.count-- : e.stateNode && (e = e.stateNode, e.parentNode.removeChild(e));
      break;
    case 27:
      il || at(e, t);
      var a = ll, u = Ol;
      It(e.type) && (ll = e.stateNode, Ol = !1), st(
        l,
        t,
        e
      ), Qa(e.stateNode), ll = a, Ol = u;
      break;
    case 5:
      il || at(e, t);
    case 6:
      if (a = ll, u = Ol, ll = null, st(
        l,
        t,
        e
      ), ll = a, Ol = u, ll !== null)
        if (Ol)
          try {
            (ll.nodeType === 9 ? ll.body : ll.nodeName === "HTML" ? ll.ownerDocument.body : ll).removeChild(e.stateNode);
          } catch (n) {
            V(
              e,
              t,
              n
            );
          }
        else
          try {
            ll.removeChild(e.stateNode);
          } catch (n) {
            V(
              e,
              t,
              n
            );
          }
      break;
    case 18:
      ll !== null && (Ol ? (l = ll, Fs(
        l.nodeType === 9 ? l.body : l.nodeName === "HTML" ? l.ownerDocument.body : l,
        e.stateNode
      ), ea(l)) : Fs(ll, e.stateNode));
      break;
    case 4:
      a = ll, u = Ol, ll = e.stateNode.containerInfo, Ol = !0, st(
        l,
        t,
        e
      ), ll = a, Ol = u;
      break;
    case 0:
    case 11:
    case 14:
    case 15:
      $t(2, e, t), il || $t(4, e, t), st(
        l,
        t,
        e
      );
      break;
    case 1:
      il || (at(e, t), a = e.stateNode, typeof a.componentWillUnmount == "function" && Ad(
        e,
        t,
        a
      )), st(
        l,
        t,
        e
      );
      break;
    case 21:
      st(
        l,
        t,
        e
      );
      break;
    case 22:
      il = (a = il) || e.memoizedState !== null, st(
        l,
        t,
        e
      ), il = a;
      break;
    default:
      st(
        l,
        t,
        e
      );
  }
}
function Ud(l, t) {
  if (t.memoizedState === null && (l = t.alternate, l !== null && (l = l.memoizedState, l !== null))) {
    l = l.dehydrated;
    try {
      ea(l);
    } catch (e) {
      V(t, t.return, e);
    }
  }
}
function Cd(l, t) {
  if (t.memoizedState === null && (l = t.alternate, l !== null && (l = l.memoizedState, l !== null && (l = l.dehydrated, l !== null))))
    try {
      ea(l);
    } catch (e) {
      V(t, t.return, e);
    }
}
function H0(l) {
  switch (l.tag) {
    case 31:
    case 13:
    case 19:
      var t = l.stateNode;
      return t === null && (t = l.stateNode = new qs()), t;
    case 22:
      return l = l.stateNode, t = l._retryCache, t === null && (t = l._retryCache = new qs()), t;
    default:
      throw Error(b(435, l.tag));
  }
}
function xu(l, t) {
  var e = H0(l);
  t.forEach(function(a) {
    if (!e.has(a)) {
      e.add(a);
      var u = Z0.bind(null, l, a);
      a.then(u, u);
    }
  });
}
function Al(l, t) {
  var e = t.deletions;
  if (e !== null)
    for (var a = 0; a < e.length; a++) {
      var u = e[a], n = l, i = t, c = i;
      l: for (; c !== null; ) {
        switch (c.tag) {
          case 27:
            if (It(c.type)) {
              ll = c.stateNode, Ol = !1;
              break l;
            }
            break;
          case 5:
            ll = c.stateNode, Ol = !1;
            break l;
          case 3:
          case 4:
            ll = c.stateNode.containerInfo, Ol = !0;
            break l;
        }
        c = c.return;
      }
      if (ll === null) throw Error(b(160));
      jd(n, i, u), ll = null, Ol = !1, n = u.alternate, n !== null && (n.return = null), u.return = null;
    }
  if (t.subtreeFlags & 13886)
    for (t = t.child; t !== null; )
      Hd(t, l), t = t.sibling;
}
var Pl = null;
function Hd(l, t) {
  var e = l.alternate, a = l.flags;
  switch (l.tag) {
    case 0:
    case 11:
    case 14:
    case 15:
      Al(t, l), Ml(l), a & 4 && ($t(3, l, l.return), su(3, l), $t(5, l, l.return));
      break;
    case 1:
      Al(t, l), Ml(l), a & 512 && (il || e === null || at(e, e.return)), a & 64 && mt && (l = l.updateQueue, l !== null && (a = l.callbacks, a !== null && (e = l.shared.hiddenCallbacks, l.shared.hiddenCallbacks = e === null ? a : e.concat(a))));
      break;
    case 26:
      var u = Pl;
      if (Al(t, l), Ml(l), a & 512 && (il || e === null || at(e, e.return)), a & 4) {
        var n = e !== null ? e.memoizedState : null;
        if (a = l.memoizedState, e === null)
          if (a === null)
            if (l.stateNode === null) {
              l: {
                a = l.type, e = l.memoizedProps, u = u.ownerDocument || u;
                t: switch (a) {
                  case "title":
                    n = u.getElementsByTagName("title")[0], (!n || n[nu] || n[gl] || n.namespaceURI === "http://www.w3.org/2000/svg" || n.hasAttribute("itemprop")) && (n = u.createElement(a), u.head.insertBefore(
                      n,
                      u.querySelector("head > title")
                    )), _l(n, a, e), n[gl] = l, hl(n), a = n;
                    break l;
                  case "link":
                    var i = uo(
                      "link",
                      "href",
                      u
                    ).get(a + (e.href || ""));
                    if (i) {
                      for (var c = 0; c < i.length; c++)
                        if (n = i[c], n.getAttribute("href") === (e.href == null || e.href === "" ? null : e.href) && n.getAttribute("rel") === (e.rel == null ? null : e.rel) && n.getAttribute("title") === (e.title == null ? null : e.title) && n.getAttribute("crossorigin") === (e.crossOrigin == null ? null : e.crossOrigin)) {
                          i.splice(c, 1);
                          break t;
                        }
                    }
                    n = u.createElement(a), _l(n, a, e), u.head.appendChild(n);
                    break;
                  case "meta":
                    if (i = uo(
                      "meta",
                      "content",
                      u
                    ).get(a + (e.content || ""))) {
                      for (c = 0; c < i.length; c++)
                        if (n = i[c], n.getAttribute("content") === (e.content == null ? null : "" + e.content) && n.getAttribute("name") === (e.name == null ? null : e.name) && n.getAttribute("property") === (e.property == null ? null : e.property) && n.getAttribute("http-equiv") === (e.httpEquiv == null ? null : e.httpEquiv) && n.getAttribute("charset") === (e.charSet == null ? null : e.charSet)) {
                          i.splice(c, 1);
                          break t;
                        }
                    }
                    n = u.createElement(a), _l(n, a, e), u.head.appendChild(n);
                    break;
                  default:
                    throw Error(b(468, a));
                }
                n[gl] = l, hl(n), a = n;
              }
              l.stateNode = a;
            } else
              no(
                u,
                l.type,
                l.stateNode
              );
          else
            l.stateNode = ao(
              u,
              a,
              l.memoizedProps
            );
        else
          n !== a ? (n === null ? e.stateNode !== null && (e = e.stateNode, e.parentNode.removeChild(e)) : n.count--, a === null ? no(
            u,
            l.type,
            l.stateNode
          ) : ao(
            u,
            a,
            l.memoizedProps
          )) : a === null && l.stateNode !== null && bi(
            l,
            l.memoizedProps,
            e.memoizedProps
          );
      }
      break;
    case 27:
      Al(t, l), Ml(l), a & 512 && (il || e === null || at(e, e.return)), e !== null && a & 4 && bi(
        l,
        l.memoizedProps,
        e.memoizedProps
      );
      break;
    case 5:
      if (Al(t, l), Ml(l), a & 512 && (il || e === null || at(e, e.return)), l.flags & 32) {
        u = l.stateNode;
        try {
          ke(u, "");
        } catch (p) {
          V(l, l.return, p);
        }
      }
      a & 4 && l.stateNode != null && (u = l.memoizedProps, bi(
        l,
        u,
        e !== null ? e.memoizedProps : u
      )), a & 1024 && (Ei = !0);
      break;
    case 6:
      if (Al(t, l), Ml(l), a & 4) {
        if (l.stateNode === null)
          throw Error(b(162));
        a = l.memoizedProps, e = l.stateNode;
        try {
          e.nodeValue = a;
        } catch (p) {
          V(l, l.return, p);
        }
      }
      break;
    case 3:
      if (Ku = null, u = Pl, Pl = _n(t.containerInfo), Al(t, l), Pl = u, Ml(l), a & 4 && e !== null && e.memoizedState.isDehydrated)
        try {
          ea(t.containerInfo);
        } catch (p) {
          V(l, l.return, p);
        }
      Ei && (Ei = !1, Rd(l));
      break;
    case 4:
      a = Pl, Pl = _n(
        l.stateNode.containerInfo
      ), Al(t, l), Ml(l), Pl = a;
      break;
    case 12:
      Al(t, l), Ml(l);
      break;
    case 31:
      Al(t, l), Ml(l), a & 4 && (a = l.updateQueue, a !== null && (l.updateQueue = null, xu(l, a)));
      break;
    case 13:
      Al(t, l), Ml(l), l.child.flags & 8192 && l.memoizedState !== null != (e !== null && e.memoizedState !== null) && (Qn = Bl()), a & 4 && (a = l.updateQueue, a !== null && (l.updateQueue = null, xu(l, a)));
      break;
    case 22:
      u = l.memoizedState !== null;
      var f = e !== null && e.memoizedState !== null, o = mt, d = il;
      if (mt = o || u, il = d || f, Al(t, l), il = d, mt = o, Ml(l), a & 8192)
        l: for (t = l.stateNode, t._visibility = u ? t._visibility & -2 : t._visibility | 1, u && (e === null || f || mt || il || ue(l)), e = null, t = l; ; ) {
          if (t.tag === 5 || t.tag === 26) {
            if (e === null) {
              f = e = t;
              try {
                if (n = f.stateNode, u)
                  i = n.style, typeof i.setProperty == "function" ? i.setProperty("display", "none", "important") : i.display = "none";
                else {
                  c = f.stateNode;
                  var y = f.memoizedProps.style, h = y != null && y.hasOwnProperty("display") ? y.display : null;
                  c.style.display = h == null || typeof h == "boolean" ? "" : ("" + h).trim();
                }
              } catch (p) {
                V(f, f.return, p);
              }
            }
          } else if (t.tag === 6) {
            if (e === null) {
              f = t;
              try {
                f.stateNode.nodeValue = u ? "" : f.memoizedProps;
              } catch (p) {
                V(f, f.return, p);
              }
            }
          } else if (t.tag === 18) {
            if (e === null) {
              f = t;
              try {
                var v = f.stateNode;
                u ? Is(v, !0) : Is(f.stateNode, !1);
              } catch (p) {
                V(f, f.return, p);
              }
            }
          } else if ((t.tag !== 22 && t.tag !== 23 || t.memoizedState === null || t === l) && t.child !== null) {
            t.child.return = t, t = t.child;
            continue;
          }
          if (t === l) break l;
          for (; t.sibling === null; ) {
            if (t.return === null || t.return === l) break l;
            e === t && (e = null), t = t.return;
          }
          e === t && (e = null), t.sibling.return = t.return, t = t.sibling;
        }
      a & 4 && (a = l.updateQueue, a !== null && (e = a.retryQueue, e !== null && (a.retryQueue = null, xu(l, e))));
      break;
    case 19:
      Al(t, l), Ml(l), a & 4 && (a = l.updateQueue, a !== null && (l.updateQueue = null, xu(l, a)));
      break;
    case 30:
      break;
    case 21:
      break;
    default:
      Al(t, l), Ml(l);
  }
}
function Ml(l) {
  var t = l.flags;
  if (t & 2) {
    try {
      for (var e, a = l.return; a !== null; ) {
        if (Od(a)) {
          e = a;
          break;
        }
        a = a.return;
      }
      if (e == null) throw Error(b(160));
      switch (e.tag) {
        case 27:
          var u = e.stateNode, n = _i(l);
          dn(l, n, u);
          break;
        case 5:
          var i = e.stateNode;
          e.flags & 32 && (ke(i, ""), e.flags &= -33);
          var c = _i(l);
          dn(l, c, i);
          break;
        case 3:
        case 4:
          var f = e.stateNode.containerInfo, o = _i(l);
          dc(
            l,
            o,
            f
          );
          break;
        default:
          throw Error(b(161));
      }
    } catch (d) {
      V(l, l.return, d);
    }
    l.flags &= -3;
  }
  t & 4096 && (l.flags &= -4097);
}
function Rd(l) {
  if (l.subtreeFlags & 1024)
    for (l = l.child; l !== null; ) {
      var t = l;
      Rd(t), t.tag === 5 && t.flags & 1024 && t.stateNode.reset(), l = l.sibling;
    }
}
function ot(l, t) {
  if (t.subtreeFlags & 8772)
    for (t = t.child; t !== null; )
      xd(l, t.alternate, t), t = t.sibling;
}
function ue(l) {
  for (l = l.child; l !== null; ) {
    var t = l;
    switch (t.tag) {
      case 0:
      case 11:
      case 14:
      case 15:
        $t(4, t, t.return), ue(t);
        break;
      case 1:
        at(t, t.return);
        var e = t.stateNode;
        typeof e.componentWillUnmount == "function" && Ad(
          t,
          t.return,
          e
        ), ue(t);
        break;
      case 27:
        Qa(t.stateNode);
      case 26:
      case 5:
        at(t, t.return), ue(t);
        break;
      case 22:
        t.memoizedState === null && ue(t);
        break;
      case 30:
        ue(t);
        break;
      default:
        ue(t);
    }
    l = l.sibling;
  }
}
function rt(l, t, e) {
  for (e = e && (t.subtreeFlags & 8772) !== 0, t = t.child; t !== null; ) {
    var a = t.alternate, u = l, n = t, i = n.flags;
    switch (n.tag) {
      case 0:
      case 11:
      case 15:
        rt(
          u,
          n,
          e
        ), su(4, n);
        break;
      case 1:
        if (rt(
          u,
          n,
          e
        ), a = n, u = a.stateNode, typeof u.componentDidMount == "function")
          try {
            u.componentDidMount();
          } catch (o) {
            V(a, a.return, o);
          }
        if (a = n, u = a.updateQueue, u !== null) {
          var c = a.stateNode;
          try {
            var f = u.shared.hiddenCallbacks;
            if (f !== null)
              for (u.shared.hiddenCallbacks = null, u = 0; u < f.length; u++)
                Ur(f[u], c);
          } catch (o) {
            V(a, a.return, o);
          }
        }
        e && i & 64 && zd(n), Ba(n, n.return);
        break;
      case 27:
        Dd(n);
      case 26:
      case 5:
        rt(
          u,
          n,
          e
        ), e && a === null && i & 4 && Md(n), Ba(n, n.return);
        break;
      case 12:
        rt(
          u,
          n,
          e
        );
        break;
      case 31:
        rt(
          u,
          n,
          e
        ), e && i & 4 && Ud(u, n);
        break;
      case 13:
        rt(
          u,
          n,
          e
        ), e && i & 4 && Cd(u, n);
        break;
      case 22:
        n.memoizedState === null && rt(
          u,
          n,
          e
        ), Ba(n, n.return);
        break;
      case 30:
        break;
      default:
        rt(
          u,
          n,
          e
        );
    }
    t = t.sibling;
  }
}
function bf(l, t) {
  var e = null;
  l !== null && l.memoizedState !== null && l.memoizedState.cachePool !== null && (e = l.memoizedState.cachePool.pool), l = null, t.memoizedState !== null && t.memoizedState.cachePool !== null && (l = t.memoizedState.cachePool.pool), l !== e && (l != null && l.refCount++, e != null && cu(e));
}
function _f(l, t) {
  l = null, t.alternate !== null && (l = t.alternate.memoizedState.cache), t = t.memoizedState.cache, t !== l && (t.refCount++, l != null && cu(l));
}
function Il(l, t, e, a) {
  if (t.subtreeFlags & 10256)
    for (t = t.child; t !== null; )
      Bd(
        l,
        t,
        e,
        a
      ), t = t.sibling;
}
function Bd(l, t, e, a) {
  var u = t.flags;
  switch (t.tag) {
    case 0:
    case 11:
    case 15:
      Il(
        l,
        t,
        e,
        a
      ), u & 2048 && su(9, t);
      break;
    case 1:
      Il(
        l,
        t,
        e,
        a
      );
      break;
    case 3:
      Il(
        l,
        t,
        e,
        a
      ), u & 2048 && (l = null, t.alternate !== null && (l = t.alternate.memoizedState.cache), t = t.memoizedState.cache, t !== l && (t.refCount++, l != null && cu(l)));
      break;
    case 12:
      if (u & 2048) {
        Il(
          l,
          t,
          e,
          a
        ), l = t.stateNode;
        try {
          var n = t.memoizedProps, i = n.id, c = n.onPostCommit;
          typeof c == "function" && c(
            i,
            t.alternate === null ? "mount" : "update",
            l.passiveEffectDuration,
            -0
          );
        } catch (f) {
          V(t, t.return, f);
        }
      } else
        Il(
          l,
          t,
          e,
          a
        );
      break;
    case 31:
      Il(
        l,
        t,
        e,
        a
      );
      break;
    case 13:
      Il(
        l,
        t,
        e,
        a
      );
      break;
    case 23:
      break;
    case 22:
      n = t.stateNode, i = t.alternate, t.memoizedState !== null ? n._visibility & 2 ? Il(
        l,
        t,
        e,
        a
      ) : Ya(l, t) : n._visibility & 2 ? Il(
        l,
        t,
        e,
        a
      ) : (n._visibility |= 2, Ae(
        l,
        t,
        e,
        a,
        (t.subtreeFlags & 10256) !== 0 || !1
      )), u & 2048 && bf(i, t);
      break;
    case 24:
      Il(
        l,
        t,
        e,
        a
      ), u & 2048 && _f(t.alternate, t);
      break;
    default:
      Il(
        l,
        t,
        e,
        a
      );
  }
}
function Ae(l, t, e, a, u) {
  for (u = u && ((t.subtreeFlags & 10256) !== 0 || !1), t = t.child; t !== null; ) {
    var n = l, i = t, c = e, f = a, o = i.flags;
    switch (i.tag) {
      case 0:
      case 11:
      case 15:
        Ae(
          n,
          i,
          c,
          f,
          u
        ), su(8, i);
        break;
      case 23:
        break;
      case 22:
        var d = i.stateNode;
        i.memoizedState !== null ? d._visibility & 2 ? Ae(
          n,
          i,
          c,
          f,
          u
        ) : Ya(
          n,
          i
        ) : (d._visibility |= 2, Ae(
          n,
          i,
          c,
          f,
          u
        )), u && o & 2048 && bf(
          i.alternate,
          i
        );
        break;
      case 24:
        Ae(
          n,
          i,
          c,
          f,
          u
        ), u && o & 2048 && _f(i.alternate, i);
        break;
      default:
        Ae(
          n,
          i,
          c,
          f,
          u
        );
    }
    t = t.sibling;
  }
}
function Ya(l, t) {
  if (t.subtreeFlags & 10256)
    for (t = t.child; t !== null; ) {
      var e = l, a = t, u = a.flags;
      switch (a.tag) {
        case 22:
          Ya(e, a), u & 2048 && bf(
            a.alternate,
            a
          );
          break;
        case 24:
          Ya(e, a), u & 2048 && _f(a.alternate, a);
          break;
        default:
          Ya(e, a);
      }
      t = t.sibling;
    }
}
var Ma = 8192;
function pe(l, t, e) {
  if (l.subtreeFlags & Ma)
    for (l = l.child; l !== null; )
      Yd(
        l,
        t,
        e
      ), l = l.sibling;
}
function Yd(l, t, e) {
  switch (l.tag) {
    case 26:
      pe(
        l,
        t,
        e
      ), l.flags & Ma && l.memoizedState !== null && _y(
        e,
        Pl,
        l.memoizedState,
        l.memoizedProps
      );
      break;
    case 5:
      pe(
        l,
        t,
        e
      );
      break;
    case 3:
    case 4:
      var a = Pl;
      Pl = _n(l.stateNode.containerInfo), pe(
        l,
        t,
        e
      ), Pl = a;
      break;
    case 22:
      l.memoizedState === null && (a = l.alternate, a !== null && a.memoizedState !== null ? (a = Ma, Ma = 16777216, pe(
        l,
        t,
        e
      ), Ma = a) : pe(
        l,
        t,
        e
      ));
      break;
    default:
      pe(
        l,
        t,
        e
      );
  }
}
function qd(l) {
  var t = l.alternate;
  if (t !== null && (l = t.child, l !== null)) {
    t.child = null;
    do
      t = l.sibling, l.sibling = null, l = t;
    while (l !== null);
  }
}
function ba(l) {
  var t = l.deletions;
  if (l.flags & 16) {
    if (t !== null)
      for (var e = 0; e < t.length; e++) {
        var a = t[e];
        ml = a, Qd(
          a,
          l
        );
      }
    qd(l);
  }
  if (l.subtreeFlags & 10256)
    for (l = l.child; l !== null; )
      Gd(l), l = l.sibling;
}
function Gd(l) {
  switch (l.tag) {
    case 0:
    case 11:
    case 15:
      ba(l), l.flags & 2048 && $t(9, l, l.return);
      break;
    case 3:
      ba(l);
      break;
    case 12:
      ba(l);
      break;
    case 22:
      var t = l.stateNode;
      l.memoizedState !== null && t._visibility & 2 && (l.return === null || l.return.tag !== 13) ? (t._visibility &= -3, wu(l)) : ba(l);
      break;
    default:
      ba(l);
  }
}
function wu(l) {
  var t = l.deletions;
  if (l.flags & 16) {
    if (t !== null)
      for (var e = 0; e < t.length; e++) {
        var a = t[e];
        ml = a, Qd(
          a,
          l
        );
      }
    qd(l);
  }
  for (l = l.child; l !== null; ) {
    switch (t = l, t.tag) {
      case 0:
      case 11:
      case 15:
        $t(8, t, t.return), wu(t);
        break;
      case 22:
        e = t.stateNode, e._visibility & 2 && (e._visibility &= -3, wu(t));
        break;
      default:
        wu(t);
    }
    l = l.sibling;
  }
}
function Qd(l, t) {
  for (; ml !== null; ) {
    var e = ml;
    switch (e.tag) {
      case 0:
      case 11:
      case 15:
        $t(8, e, t);
        break;
      case 23:
      case 22:
        if (e.memoizedState !== null && e.memoizedState.cachePool !== null) {
          var a = e.memoizedState.cachePool.pool;
          a != null && a.refCount++;
        }
        break;
      case 24:
        cu(e.memoizedState.cache);
    }
    if (a = e.child, a !== null) a.return = e, ml = a;
    else
      l: for (e = l; ml !== null; ) {
        a = ml;
        var u = a.sibling, n = a.return;
        if (Nd(a), a === e) {
          ml = null;
          break l;
        }
        if (u !== null) {
          u.return = n, ml = u;
          break l;
        }
        ml = n;
      }
  }
}
var R0 = {
  getCacheForType: function(l) {
    var t = bl(cl), e = t.data.get(l);
    return e === void 0 && (e = l(), t.data.set(l, e)), e;
  },
  cacheSignal: function() {
    return bl(cl).controller.signal;
  }
}, B0 = typeof WeakMap == "function" ? WeakMap : Map, L = 0, k = null, q = null, G = 0, w = 0, Ul = null, Rt = !1, sa = !1, Ef = !1, zt = 0, el = 0, Wt = 0, se = 0, pf = 0, Rl = 0, Ie = 0, qa = null, Dl = null, mc = !1, Qn = 0, Xd = 0, mn = 1 / 0, hn = null, Lt = null, sl = 0, Zt = null, Pe = null, bt = 0, hc = 0, yc = null, Ld = null, Ga = 0, vc = null;
function Gl() {
  return L & 2 && G !== 0 ? G & -G : C.T !== null ? zf() : Fo();
}
function Zd() {
  if (Rl === 0)
    if (!(G & 536870912) || Q) {
      var l = Eu;
      Eu <<= 1, !(Eu & 3932160) && (Eu = 262144), Rl = l;
    } else Rl = 536870912;
  return l = Xl.current, l !== null && (l.flags |= 32), Rl;
}
function xl(l, t, e) {
  (l === k && (w === 2 || w === 9) || l.cancelPendingCommit !== null) && (la(l, 0), Bt(
    l,
    G,
    Rl,
    !1
  )), uu(l, e), (!(L & 2) || l !== k) && (l === k && (!(L & 2) && (se |= e), el === 4 && Bt(
    l,
    G,
    Rl,
    !1
  )), it(l));
}
function wd(l, t, e) {
  if (L & 6) throw Error(b(327));
  var a = !e && (t & 127) === 0 && (t & l.expiredLanes) === 0 || au(l, t), u = a ? G0(l, t) : pi(l, t, !0), n = a;
  do {
    if (u === 0) {
      sa && !a && Bt(l, t, 0, !1);
      break;
    } else {
      if (e = l.current.alternate, n && !Y0(e)) {
        u = pi(l, t, !1), n = !1;
        continue;
      }
      if (u === 2) {
        if (n = t, l.errorRecoveryDisabledLanes & n)
          var i = 0;
        else
          i = l.pendingLanes & -536870913, i = i !== 0 ? i : i & 536870912 ? 536870912 : 0;
        if (i !== 0) {
          t = i;
          l: {
            var c = l;
            u = qa;
            var f = c.current.memoizedState.isDehydrated;
            if (f && (la(c, i).flags |= 256), i = pi(
              c,
              i,
              !1
            ), i !== 2) {
              if (Ef && !f) {
                c.errorRecoveryDisabledLanes |= n, se |= n, u = 4;
                break l;
              }
              n = Dl, Dl = u, n !== null && (Dl === null ? Dl = n : Dl.push.apply(
                Dl,
                n
              ));
            }
            u = i;
          }
          if (n = !1, u !== 2) continue;
        }
      }
      if (u === 1) {
        la(l, 0), Bt(l, t, 0, !0);
        break;
      }
      l: {
        switch (a = l, n = u, n) {
          case 0:
          case 1:
            throw Error(b(345));
          case 4:
            if ((t & 4194048) !== t) break;
          case 6:
            Bt(
              a,
              t,
              Rl,
              !Rt
            );
            break l;
          case 2:
            Dl = null;
            break;
          case 3:
          case 5:
            break;
          default:
            throw Error(b(329));
        }
        if ((t & 62914560) === t && (u = Qn + 300 - Bl(), 10 < u)) {
          if (Bt(
            a,
            t,
            Rl,
            !Rt
          ), xn(a, 0, !0) !== 0) break l;
          bt = t, a.timeoutHandle = om(
            Gs.bind(
              null,
              a,
              e,
              Dl,
              hn,
              mc,
              t,
              Rl,
              se,
              Ie,
              Rt,
              n,
              "Throttled",
              -0,
              0
            ),
            u
          );
          break l;
        }
        Gs(
          a,
          e,
          Dl,
          hn,
          mc,
          t,
          Rl,
          se,
          Ie,
          Rt,
          n,
          null,
          -0,
          0
        );
      }
    }
    break;
  } while (!0);
  it(l);
}
function Gs(l, t, e, a, u, n, i, c, f, o, d, y, h, v) {
  if (l.timeoutHandle = -1, y = t.subtreeFlags, y & 8192 || (y & 16785408) === 16785408) {
    y = {
      stylesheets: null,
      count: 0,
      imgCount: 0,
      imgBytes: 0,
      suspenseyImages: [],
      waitingForImages: !0,
      waitingForViewTransition: !1,
      unsuspend: yt
    }, Yd(
      t,
      n,
      y
    );
    var p = (n & 62914560) === n ? Qn - Bl() : (n & 4194048) === n ? Xd - Bl() : 0;
    if (p = Ey(
      y,
      p
    ), p !== null) {
      bt = n, l.cancelPendingCommit = p(
        Xs.bind(
          null,
          l,
          t,
          n,
          e,
          a,
          u,
          i,
          c,
          f,
          d,
          y,
          null,
          h,
          v
        )
      ), Bt(l, n, i, !o);
      return;
    }
  }
  Xs(
    l,
    t,
    n,
    e,
    a,
    u,
    i,
    c,
    f
  );
}
function Y0(l) {
  for (var t = l; ; ) {
    var e = t.tag;
    if ((e === 0 || e === 11 || e === 15) && t.flags & 16384 && (e = t.updateQueue, e !== null && (e = e.stores, e !== null)))
      for (var a = 0; a < e.length; a++) {
        var u = e[a], n = u.getSnapshot;
        u = u.value;
        try {
          if (!Ql(n(), u)) return !1;
        } catch {
          return !1;
        }
      }
    if (e = t.child, t.subtreeFlags & 16384 && e !== null)
      e.return = t, t = e;
    else {
      if (t === l) break;
      for (; t.sibling === null; ) {
        if (t.return === null || t.return === l) return !0;
        t = t.return;
      }
      t.sibling.return = t.return, t = t.sibling;
    }
  }
  return !0;
}
function Bt(l, t, e, a) {
  t &= ~pf, t &= ~se, l.suspendedLanes |= t, l.pingedLanes &= ~t, a && (l.warmLanes |= t), a = l.expirationTimes;
  for (var u = t; 0 < u; ) {
    var n = 31 - ql(u), i = 1 << n;
    a[n] = -1, u &= ~i;
  }
  e !== 0 && ko(l, e, t);
}
function Xn() {
  return L & 6 ? !0 : (ou(0), !1);
}
function Tf() {
  if (q !== null) {
    if (w === 0)
      var l = q.return;
    else
      l = q, vt = be = null, ff(l), Ze = null, Ja = 0, l = q;
    for (; l !== null; )
      Td(l.alternate, l), l = l.return;
    q = null;
  }
}
function la(l, t) {
  var e = l.timeoutHandle;
  e !== -1 && (l.timeoutHandle = -1, ay(e)), e = l.cancelPendingCommit, e !== null && (l.cancelPendingCommit = null, e()), bt = 0, Tf(), k = l, q = e = gt(l.current, null), G = t, w = 0, Ul = null, Rt = !1, sa = au(l, t), Ef = !1, Ie = Rl = pf = se = Wt = el = 0, Dl = qa = null, mc = !1, t & 8 && (t |= t & 32);
  var a = l.entangledLanes;
  if (a !== 0)
    for (l = l.entanglements, a &= t; 0 < a; ) {
      var u = 31 - ql(a), n = 1 << u;
      t |= l[u], a &= ~n;
    }
  return zt = t, Cn(), e;
}
function Vd(l, t) {
  R = null, C.H = $a, t === fa || t === Rn ? (t = gs(), w = 3) : t === lf ? (t = gs(), w = 4) : w = t === gf ? 8 : t !== null && typeof t == "object" && typeof t.then == "function" ? 6 : 1, Ul = t, q === null && (el = 1, on(
    l,
    Jl(t, l.current)
  ));
}
function Kd() {
  var l = Xl.current;
  return l === null ? !0 : (G & 4194048) === G ? $l === null : (G & 62914560) === G || G & 536870912 ? l === $l : !1;
}
function Jd() {
  var l = C.H;
  return C.H = $a, l === null ? $a : l;
}
function kd() {
  var l = C.A;
  return C.A = R0, l;
}
function yn() {
  el = 4, Rt || (G & 4194048) !== G && Xl.current !== null || (sa = !0), !(Wt & 134217727) && !(se & 134217727) || k === null || Bt(
    k,
    G,
    Rl,
    !1
  );
}
function pi(l, t, e) {
  var a = L;
  L |= 2;
  var u = Jd(), n = kd();
  (k !== l || G !== t) && (hn = null, la(l, t)), t = !1;
  var i = el;
  l: do
    try {
      if (w !== 0 && q !== null) {
        var c = q, f = Ul;
        switch (w) {
          case 8:
            Tf(), i = 6;
            break l;
          case 3:
          case 2:
          case 9:
          case 6:
            Xl.current === null && (t = !0);
            var o = w;
            if (w = 0, Ul = null, qe(l, c, f, o), e && sa) {
              i = 0;
              break l;
            }
            break;
          default:
            o = w, w = 0, Ul = null, qe(l, c, f, o);
        }
      }
      q0(), i = el;
      break;
    } catch (d) {
      Vd(l, d);
    }
  while (!0);
  return t && l.shellSuspendCounter++, vt = be = null, L = a, C.H = u, C.A = n, q === null && (k = null, G = 0, Cn()), i;
}
function q0() {
  for (; q !== null; ) $d(q);
}
function G0(l, t) {
  var e = L;
  L |= 2;
  var a = Jd(), u = kd();
  k !== l || G !== t ? (hn = null, mn = Bl() + 500, la(l, t)) : sa = au(
    l,
    t
  );
  l: do
    try {
      if (w !== 0 && q !== null) {
        t = q;
        var n = Ul;
        t: switch (w) {
          case 1:
            w = 0, Ul = null, qe(l, t, n, 1);
            break;
          case 2:
          case 9:
            if (vs(n)) {
              w = 0, Ul = null, Qs(t);
              break;
            }
            t = function() {
              w !== 2 && w !== 9 || k !== l || (w = 7), it(l);
            }, n.then(t, t);
            break l;
          case 3:
            w = 7;
            break l;
          case 4:
            w = 5;
            break l;
          case 7:
            vs(n) ? (w = 0, Ul = null, Qs(t)) : (w = 0, Ul = null, qe(l, t, n, 7));
            break;
          case 5:
            var i = null;
            switch (q.tag) {
              case 26:
                i = q.memoizedState;
              case 5:
              case 27:
                var c = q;
                if (i ? ym(i) : c.stateNode.complete) {
                  w = 0, Ul = null;
                  var f = c.sibling;
                  if (f !== null) q = f;
                  else {
                    var o = c.return;
                    o !== null ? (q = o, Ln(o)) : q = null;
                  }
                  break t;
                }
            }
            w = 0, Ul = null, qe(l, t, n, 5);
            break;
          case 6:
            w = 0, Ul = null, qe(l, t, n, 6);
            break;
          case 8:
            Tf(), el = 6;
            break l;
          default:
            throw Error(b(462));
        }
      }
      Q0();
      break;
    } catch (d) {
      Vd(l, d);
    }
  while (!0);
  return vt = be = null, C.H = a, C.A = u, L = e, q !== null ? 0 : (k = null, G = 0, Cn(), el);
}
function Q0() {
  for (; q !== null && !sh(); )
    $d(q);
}
function $d(l) {
  var t = pd(l.alternate, l, zt);
  l.memoizedProps = l.pendingProps, t === null ? Ln(l) : q = t;
}
function Qs(l) {
  var t = l, e = t.alternate;
  switch (t.tag) {
    case 15:
    case 0:
      t = Cs(
        e,
        t,
        t.pendingProps,
        t.type,
        void 0,
        G
      );
      break;
    case 11:
      t = Cs(
        e,
        t,
        t.pendingProps,
        t.type.render,
        t.ref,
        G
      );
      break;
    case 5:
      ff(t);
    default:
      Td(e, t), t = q = Tr(t, zt), t = pd(e, t, zt);
  }
  l.memoizedProps = l.pendingProps, t === null ? Ln(l) : q = t;
}
function qe(l, t, e, a) {
  vt = be = null, ff(t), Ze = null, Ja = 0;
  var u = t.return;
  try {
    if (D0(
      l,
      u,
      t,
      e,
      G
    )) {
      el = 1, on(
        l,
        Jl(e, l.current)
      ), q = null;
      return;
    }
  } catch (n) {
    if (u !== null) throw q = u, n;
    el = 1, on(
      l,
      Jl(e, l.current)
    ), q = null;
    return;
  }
  t.flags & 32768 ? (Q || a === 1 ? l = !0 : sa || G & 536870912 ? l = !1 : (Rt = l = !0, (a === 2 || a === 9 || a === 3 || a === 6) && (a = Xl.current, a !== null && a.tag === 13 && (a.flags |= 16384))), Wd(t, l)) : Ln(t);
}
function Ln(l) {
  var t = l;
  do {
    if (t.flags & 32768) {
      Wd(
        t,
        Rt
      );
      return;
    }
    l = t.return;
    var e = j0(
      t.alternate,
      t,
      zt
    );
    if (e !== null) {
      q = e;
      return;
    }
    if (t = t.sibling, t !== null) {
      q = t;
      return;
    }
    q = t = l;
  } while (t !== null);
  el === 0 && (el = 5);
}
function Wd(l, t) {
  do {
    var e = U0(l.alternate, l);
    if (e !== null) {
      e.flags &= 32767, q = e;
      return;
    }
    if (e = l.return, e !== null && (e.flags |= 32768, e.subtreeFlags = 0, e.deletions = null), !t && (l = l.sibling, l !== null)) {
      q = l;
      return;
    }
    q = l = e;
  } while (l !== null);
  el = 6, q = null;
}
function Xs(l, t, e, a, u, n, i, c, f) {
  l.cancelPendingCommit = null;
  do
    Zn();
  while (sl !== 0);
  if (L & 6) throw Error(b(327));
  if (t !== null) {
    if (t === l.current) throw Error(b(177));
    if (n = t.lanes | t.childLanes, n |= Jc, bh(
      l,
      e,
      n,
      i,
      c,
      f
    ), l === k && (q = k = null, G = 0), Pe = t, Zt = l, bt = e, hc = n, yc = u, Ld = a, t.subtreeFlags & 10256 || t.flags & 10256 ? (l.callbackNode = null, l.callbackPriority = 0, w0(Iu, function() {
      return tm(), null;
    })) : (l.callbackNode = null, l.callbackPriority = 0), a = (t.flags & 13878) !== 0, t.subtreeFlags & 13878 || a) {
      a = C.T, C.T = null, u = Z.p, Z.p = 2, i = L, L |= 4;
      try {
        C0(l, t, e);
      } finally {
        L = i, Z.p = u, C.T = a;
      }
    }
    sl = 1, Fd(), Id(), Pd();
  }
}
function Fd() {
  if (sl === 1) {
    sl = 0;
    var l = Zt, t = Pe, e = (t.flags & 13878) !== 0;
    if (t.subtreeFlags & 13878 || e) {
      e = C.T, C.T = null;
      var a = Z.p;
      Z.p = 2;
      var u = L;
      L |= 4;
      try {
        Hd(t, l);
        var n = _c, i = yr(l.containerInfo), c = n.focusedElem, f = n.selectionRange;
        if (i !== c && c && c.ownerDocument && hr(
          c.ownerDocument.documentElement,
          c
        )) {
          if (f !== null && Kc(c)) {
            var o = f.start, d = f.end;
            if (d === void 0 && (d = o), "selectionStart" in c)
              c.selectionStart = o, c.selectionEnd = Math.min(
                d,
                c.value.length
              );
            else {
              var y = c.ownerDocument || document, h = y && y.defaultView || window;
              if (h.getSelection) {
                var v = h.getSelection(), p = c.textContent.length, T = Math.min(f.start, p), E = f.end === void 0 ? T : Math.min(f.end, p);
                !v.extend && T > E && (i = E, E = T, T = i);
                var r = ss(
                  c,
                  T
                ), s = ss(
                  c,
                  E
                );
                if (r && s && (v.rangeCount !== 1 || v.anchorNode !== r.node || v.anchorOffset !== r.offset || v.focusNode !== s.node || v.focusOffset !== s.offset)) {
                  var m = y.createRange();
                  m.setStart(r.node, r.offset), v.removeAllRanges(), T > E ? (v.addRange(m), v.extend(s.node, s.offset)) : (m.setEnd(s.node, s.offset), v.addRange(m));
                }
              }
            }
          }
          for (y = [], v = c; v = v.parentNode; )
            v.nodeType === 1 && y.push({
              element: v,
              left: v.scrollLeft,
              top: v.scrollTop
            });
          for (typeof c.focus == "function" && c.focus(), c = 0; c < y.length; c++) {
            var S = y[c];
            S.element.scrollLeft = S.left, S.element.scrollTop = S.top;
          }
        }
        Tn = !!bc, _c = bc = null;
      } finally {
        L = u, Z.p = a, C.T = e;
      }
    }
    l.current = t, sl = 2;
  }
}
function Id() {
  if (sl === 2) {
    sl = 0;
    var l = Zt, t = Pe, e = (t.flags & 8772) !== 0;
    if (t.subtreeFlags & 8772 || e) {
      e = C.T, C.T = null;
      var a = Z.p;
      Z.p = 2;
      var u = L;
      L |= 4;
      try {
        xd(l, t.alternate, t);
      } finally {
        L = u, Z.p = a, C.T = e;
      }
    }
    sl = 3;
  }
}
function Pd() {
  if (sl === 4 || sl === 3) {
    sl = 0, oh();
    var l = Zt, t = Pe, e = bt, a = Ld;
    t.subtreeFlags & 10256 || t.flags & 10256 ? sl = 5 : (sl = 0, Pe = Zt = null, lm(l, l.pendingLanes));
    var u = l.pendingLanes;
    if (u === 0 && (Lt = null), Gc(e), t = t.stateNode, Yl && typeof Yl.onCommitFiberRoot == "function")
      try {
        Yl.onCommitFiberRoot(
          eu,
          t,
          void 0,
          (t.current.flags & 128) === 128
        );
      } catch {
      }
    if (a !== null) {
      t = C.T, u = Z.p, Z.p = 2, C.T = null;
      try {
        for (var n = l.onRecoverableError, i = 0; i < a.length; i++) {
          var c = a[i];
          n(c.value, {
            componentStack: c.stack
          });
        }
      } finally {
        C.T = t, Z.p = u;
      }
    }
    bt & 3 && Zn(), it(l), u = l.pendingLanes, e & 261930 && u & 42 ? l === vc ? Ga++ : (Ga = 0, vc = l) : Ga = 0, ou(0);
  }
}
function lm(l, t) {
  (l.pooledCacheLanes &= t) === 0 && (t = l.pooledCache, t != null && (l.pooledCache = null, cu(t)));
}
function Zn() {
  return Fd(), Id(), Pd(), tm();
}
function tm() {
  if (sl !== 5) return !1;
  var l = Zt, t = hc;
  hc = 0;
  var e = Gc(bt), a = C.T, u = Z.p;
  try {
    Z.p = 32 > e ? 32 : e, C.T = null, e = yc, yc = null;
    var n = Zt, i = bt;
    if (sl = 0, Pe = Zt = null, bt = 0, L & 6) throw Error(b(331));
    var c = L;
    if (L |= 4, Gd(n.current), Bd(
      n,
      n.current,
      i,
      e
    ), L = c, ou(0, !1), Yl && typeof Yl.onPostCommitFiberRoot == "function")
      try {
        Yl.onPostCommitFiberRoot(eu, n);
      } catch {
      }
    return !0;
  } finally {
    Z.p = u, C.T = a, lm(l, t);
  }
}
function Ls(l, t, e) {
  t = Jl(e, t), t = sc(l.stateNode, t, 2), l = Xt(l, t, 2), l !== null && (uu(l, 2), it(l));
}
function V(l, t, e) {
  if (l.tag === 3)
    Ls(l, l, e);
  else
    for (; t !== null; ) {
      if (t.tag === 3) {
        Ls(
          t,
          l,
          e
        );
        break;
      } else if (t.tag === 1) {
        var a = t.stateNode;
        if (typeof t.type.getDerivedStateFromError == "function" || typeof a.componentDidCatch == "function" && (Lt === null || !Lt.has(a))) {
          l = Jl(e, l), e = vd(2), a = Xt(t, e, 2), a !== null && (gd(
            e,
            a,
            t,
            l
          ), uu(a, 2), it(a));
          break;
        }
      }
      t = t.return;
    }
}
function Ti(l, t, e) {
  var a = l.pingCache;
  if (a === null) {
    a = l.pingCache = new B0();
    var u = /* @__PURE__ */ new Set();
    a.set(t, u);
  } else
    u = a.get(t), u === void 0 && (u = /* @__PURE__ */ new Set(), a.set(t, u));
  u.has(e) || (Ef = !0, u.add(e), l = X0.bind(null, l, t, e), t.then(l, l));
}
function X0(l, t, e) {
  var a = l.pingCache;
  a !== null && a.delete(t), l.pingedLanes |= l.suspendedLanes & e, l.warmLanes &= ~e, k === l && (G & e) === e && (el === 4 || el === 3 && (G & 62914560) === G && 300 > Bl() - Qn ? !(L & 2) && la(l, 0) : pf |= e, Ie === G && (Ie = 0)), it(l);
}
function em(l, t) {
  t === 0 && (t = Jo()), l = Se(l, t), l !== null && (uu(l, t), it(l));
}
function L0(l) {
  var t = l.memoizedState, e = 0;
  t !== null && (e = t.retryLane), em(l, e);
}
function Z0(l, t) {
  var e = 0;
  switch (l.tag) {
    case 31:
    case 13:
      var a = l.stateNode, u = l.memoizedState;
      u !== null && (e = u.retryLane);
      break;
    case 19:
      a = l.stateNode;
      break;
    case 22:
      a = l.stateNode._retryCache;
      break;
    default:
      throw Error(b(314));
  }
  a !== null && a.delete(t), em(l, e);
}
function w0(l, t) {
  return Yc(l, t);
}
var vn = null, Me = null, gc = !1, gn = !1, zi = !1, Yt = 0;
function it(l) {
  l !== Me && l.next === null && (Me === null ? vn = Me = l : Me = Me.next = l), gn = !0, gc || (gc = !0, K0());
}
function ou(l, t) {
  if (!zi && gn) {
    zi = !0;
    do
      for (var e = !1, a = vn; a !== null; ) {
        if (l !== 0) {
          var u = a.pendingLanes;
          if (u === 0) var n = 0;
          else {
            var i = a.suspendedLanes, c = a.pingedLanes;
            n = (1 << 31 - ql(42 | l) + 1) - 1, n &= u & ~(i & ~c), n = n & 201326741 ? n & 201326741 | 1 : n ? n | 2 : 0;
          }
          n !== 0 && (e = !0, Zs(a, n));
        } else
          n = G, n = xn(
            a,
            a === k ? n : 0,
            a.cancelPendingCommit !== null || a.timeoutHandle !== -1
          ), !(n & 3) || au(a, n) || (e = !0, Zs(a, n));
        a = a.next;
      }
    while (e);
    zi = !1;
  }
}
function V0() {
  am();
}
function am() {
  gn = gc = !1;
  var l = 0;
  Yt !== 0 && ey() && (l = Yt);
  for (var t = Bl(), e = null, a = vn; a !== null; ) {
    var u = a.next, n = um(a, t);
    n === 0 ? (a.next = null, e === null ? vn = u : e.next = u, u === null && (Me = e)) : (e = a, (l !== 0 || n & 3) && (gn = !0)), a = u;
  }
  sl !== 0 && sl !== 5 || ou(l), Yt !== 0 && (Yt = 0);
}
function um(l, t) {
  for (var e = l.suspendedLanes, a = l.pingedLanes, u = l.expirationTimes, n = l.pendingLanes & -62914561; 0 < n; ) {
    var i = 31 - ql(n), c = 1 << i, f = u[i];
    f === -1 ? (!(c & e) || c & a) && (u[i] = Sh(c, t)) : f <= t && (l.expiredLanes |= c), n &= ~c;
  }
  if (t = k, e = G, e = xn(
    l,
    l === t ? e : 0,
    l.cancelPendingCommit !== null || l.timeoutHandle !== -1
  ), a = l.callbackNode, e === 0 || l === t && (w === 2 || w === 9) || l.cancelPendingCommit !== null)
    return a !== null && a !== null && In(a), l.callbackNode = null, l.callbackPriority = 0;
  if (!(e & 3) || au(l, e)) {
    if (t = e & -e, t === l.callbackPriority) return t;
    switch (a !== null && In(a), Gc(e)) {
      case 2:
      case 8:
        e = Vo;
        break;
      case 32:
        e = Iu;
        break;
      case 268435456:
        e = Ko;
        break;
      default:
        e = Iu;
    }
    return a = nm.bind(null, l), e = Yc(e, a), l.callbackPriority = t, l.callbackNode = e, t;
  }
  return a !== null && a !== null && In(a), l.callbackPriority = 2, l.callbackNode = null, 2;
}
function nm(l, t) {
  if (sl !== 0 && sl !== 5)
    return l.callbackNode = null, l.callbackPriority = 0, null;
  var e = l.callbackNode;
  if (Zn() && l.callbackNode !== e)
    return null;
  var a = G;
  return a = xn(
    l,
    l === k ? a : 0,
    l.cancelPendingCommit !== null || l.timeoutHandle !== -1
  ), a === 0 ? null : (wd(l, a, t), um(l, Bl()), l.callbackNode != null && l.callbackNode === e ? nm.bind(null, l) : null);
}
function Zs(l, t) {
  if (Zn()) return null;
  wd(l, t, !0);
}
function K0() {
  uy(function() {
    L & 6 ? Yc(
      wo,
      V0
    ) : am();
  });
}
function zf() {
  if (Yt === 0) {
    var l = $e;
    l === 0 && (l = _u, _u <<= 1, !(_u & 261888) && (_u = 256)), Yt = l;
  }
  return Yt;
}
function ws(l) {
  return l == null || typeof l == "symbol" || typeof l == "boolean" ? null : typeof l == "function" ? l : Ru("" + l);
}
function Vs(l, t) {
  var e = t.ownerDocument.createElement("input");
  return e.name = t.name, e.value = t.value, l.id && e.setAttribute("form", l.id), t.parentNode.insertBefore(e, t), l = new FormData(l), e.parentNode.removeChild(e), l;
}
function J0(l, t, e, a, u) {
  if (t === "submit" && e && e.stateNode === u) {
    var n = ws(
      (u[Nl] || null).action
    ), i = a.submitter;
    i && (t = (t = i[Nl] || null) ? ws(t.formAction) : i.getAttribute("formAction"), t !== null && (n = t, i = null));
    var c = new Nn(
      "action",
      "action",
      null,
      a,
      u
    );
    l.push({
      event: c,
      listeners: [
        {
          instance: null,
          listener: function() {
            if (a.defaultPrevented) {
              if (Yt !== 0) {
                var f = i ? Vs(u, i) : new FormData(u);
                cc(
                  e,
                  {
                    pending: !0,
                    data: f,
                    method: u.method,
                    action: n
                  },
                  null,
                  f
                );
              }
            } else
              typeof n == "function" && (c.preventDefault(), f = i ? Vs(u, i) : new FormData(u), cc(
                e,
                {
                  pending: !0,
                  data: f,
                  method: u.method,
                  action: n
                },
                n,
                f
              ));
          },
          currentTarget: u
        }
      ]
    });
  }
}
for (var Ai = 0; Ai < $i.length; Ai++) {
  var Mi = $i[Ai], k0 = Mi.toLowerCase(), $0 = Mi[0].toUpperCase() + Mi.slice(1);
  lt(
    k0,
    "on" + $0
  );
}
lt(gr, "onAnimationEnd");
lt(Sr, "onAnimationIteration");
lt(br, "onAnimationStart");
lt("dblclick", "onDoubleClick");
lt("focusin", "onFocus");
lt("focusout", "onBlur");
lt(r0, "onTransitionRun");
lt(d0, "onTransitionStart");
lt(m0, "onTransitionCancel");
lt(_r, "onTransitionEnd");
Je("onMouseEnter", ["mouseout", "mouseover"]);
Je("onMouseLeave", ["mouseout", "mouseover"]);
Je("onPointerEnter", ["pointerout", "pointerover"]);
Je("onPointerLeave", ["pointerout", "pointerover"]);
ye(
  "onChange",
  "change click focusin focusout input keydown keyup selectionchange".split(" ")
);
ye(
  "onSelect",
  "focusout contextmenu dragend focusin keydown keyup mousedown mouseup selectionchange".split(
    " "
  )
);
ye("onBeforeInput", [
  "compositionend",
  "keypress",
  "textInput",
  "paste"
]);
ye(
  "onCompositionEnd",
  "compositionend focusout keydown keypress keyup mousedown".split(" ")
);
ye(
  "onCompositionStart",
  "compositionstart focusout keydown keypress keyup mousedown".split(" ")
);
ye(
  "onCompositionUpdate",
  "compositionupdate focusout keydown keypress keyup mousedown".split(" ")
);
var Wa = "abort canplay canplaythrough durationchange emptied encrypted ended error loadeddata loadedmetadata loadstart pause play playing progress ratechange resize seeked seeking stalled suspend timeupdate volumechange waiting".split(
  " "
), W0 = new Set(
  "beforetoggle cancel close invalid load scroll scrollend toggle".split(" ").concat(Wa)
);
function im(l, t) {
  t = (t & 4) !== 0;
  for (var e = 0; e < l.length; e++) {
    var a = l[e], u = a.event;
    a = a.listeners;
    l: {
      var n = void 0;
      if (t)
        for (var i = a.length - 1; 0 <= i; i--) {
          var c = a[i], f = c.instance, o = c.currentTarget;
          if (c = c.listener, f !== n && u.isPropagationStopped())
            break l;
          n = c, u.currentTarget = o;
          try {
            n(u);
          } catch (d) {
            ln(d);
          }
          u.currentTarget = null, n = f;
        }
      else
        for (i = 0; i < a.length; i++) {
          if (c = a[i], f = c.instance, o = c.currentTarget, c = c.listener, f !== n && u.isPropagationStopped())
            break l;
          n = c, u.currentTarget = o;
          try {
            n(u);
          } catch (d) {
            ln(d);
          }
          u.currentTarget = null, n = f;
        }
    }
  }
}
function Y(l, t) {
  var e = t[Xi];
  e === void 0 && (e = t[Xi] = /* @__PURE__ */ new Set());
  var a = l + "__bubble";
  e.has(a) || (cm(t, l, 2, !1), e.add(a));
}
function Oi(l, t, e) {
  var a = 0;
  t && (a |= 4), cm(
    e,
    l,
    a,
    t
  );
}
var Nu = "_reactListening" + Math.random().toString(36).slice(2);
function Af(l) {
  if (!l[Nu]) {
    l[Nu] = !0, Io.forEach(function(e) {
      e !== "selectionchange" && (W0.has(e) || Oi(e, !1, l), Oi(e, !0, l));
    });
    var t = l.nodeType === 9 ? l : l.ownerDocument;
    t === null || t[Nu] || (t[Nu] = !0, Oi("selectionchange", !1, t));
  }
}
function cm(l, t, e, a) {
  switch (_m(t)) {
    case 2:
      var u = zy;
      break;
    case 8:
      u = Ay;
      break;
    default:
      u = xf;
  }
  e = u.bind(
    null,
    t,
    e,
    l
  ), u = void 0, !Ki || t !== "touchstart" && t !== "touchmove" && t !== "wheel" || (u = !0), a ? u !== void 0 ? l.addEventListener(t, e, {
    capture: !0,
    passive: u
  }) : l.addEventListener(t, e, !0) : u !== void 0 ? l.addEventListener(t, e, {
    passive: u
  }) : l.addEventListener(t, e, !1);
}
function Di(l, t, e, a, u) {
  var n = a;
  if (!(t & 1) && !(t & 2) && a !== null)
    l: for (; ; ) {
      if (a === null) return;
      var i = a.tag;
      if (i === 3 || i === 4) {
        var c = a.stateNode.containerInfo;
        if (c === u) break;
        if (i === 4)
          for (i = a.return; i !== null; ) {
            var f = i.tag;
            if ((f === 3 || f === 4) && i.stateNode.containerInfo === u)
              return;
            i = i.return;
          }
        for (; c !== null; ) {
          if (i = xe(c), i === null) return;
          if (f = i.tag, f === 5 || f === 6 || f === 26 || f === 27) {
            a = n = i;
            continue l;
          }
          c = c.parentNode;
        }
      }
      a = a.return;
    }
  ir(function() {
    var o = n, d = Lc(e), y = [];
    l: {
      var h = Er.get(l);
      if (h !== void 0) {
        var v = Nn, p = l;
        switch (l) {
          case "keypress":
            if (Yu(e) === 0) break l;
          case "keydown":
          case "keyup":
            v = Zh;
            break;
          case "focusin":
            p = "focus", v = ai;
            break;
          case "focusout":
            p = "blur", v = ai;
            break;
          case "beforeblur":
          case "afterblur":
            v = ai;
            break;
          case "click":
            if (e.button === 2) break l;
          case "auxclick":
          case "dblclick":
          case "mousedown":
          case "mousemove":
          case "mouseup":
          case "mouseout":
          case "mouseover":
          case "contextmenu":
            v = Pf;
            break;
          case "drag":
          case "dragend":
          case "dragenter":
          case "dragexit":
          case "dragleave":
          case "dragover":
          case "dragstart":
          case "drop":
            v = jh;
            break;
          case "touchcancel":
          case "touchend":
          case "touchmove":
          case "touchstart":
            v = Kh;
            break;
          case gr:
          case Sr:
          case br:
            v = Hh;
            break;
          case _r:
            v = kh;
            break;
          case "scroll":
          case "scrollend":
            v = xh;
            break;
          case "wheel":
            v = Wh;
            break;
          case "copy":
          case "cut":
          case "paste":
            v = Bh;
            break;
          case "gotpointercapture":
          case "lostpointercapture":
          case "pointercancel":
          case "pointerdown":
          case "pointermove":
          case "pointerout":
          case "pointerover":
          case "pointerup":
            v = ts;
            break;
          case "toggle":
          case "beforetoggle":
            v = Ih;
        }
        var T = (t & 4) !== 0, E = !T && (l === "scroll" || l === "scrollend"), r = T ? h !== null ? h + "Capture" : null : h;
        T = [];
        for (var s = o, m; s !== null; ) {
          var S = s;
          if (m = S.stateNode, S = S.tag, S !== 5 && S !== 26 && S !== 27 || m === null || r === null || (S = La(s, r), S != null && T.push(
            Fa(s, S, m)
          )), E) break;
          s = s.return;
        }
        0 < T.length && (h = new v(
          h,
          p,
          null,
          e,
          d
        ), y.push({ event: h, listeners: T }));
      }
    }
    if (!(t & 7)) {
      l: {
        if (h = l === "mouseover" || l === "pointerover", v = l === "mouseout" || l === "pointerout", h && e !== Vi && (p = e.relatedTarget || e.fromElement) && (xe(p) || p[na]))
          break l;
        if ((v || h) && (h = d.window === d ? d : (h = d.ownerDocument) ? h.defaultView || h.parentWindow : window, v ? (p = e.relatedTarget || e.toElement, v = o, p = p ? xe(p) : null, p !== null && (E = tu(p), T = p.tag, p !== E || T !== 5 && T !== 27 && T !== 6) && (p = null)) : (v = null, p = o), v !== p)) {
          if (T = Pf, S = "onMouseLeave", r = "onMouseEnter", s = "mouse", (l === "pointerout" || l === "pointerover") && (T = ts, S = "onPointerLeave", r = "onPointerEnter", s = "pointer"), E = v == null ? h : za(v), m = p == null ? h : za(p), h = new T(
            S,
            s + "leave",
            v,
            e,
            d
          ), h.target = E, h.relatedTarget = m, S = null, xe(d) === o && (T = new T(
            r,
            s + "enter",
            p,
            e,
            d
          ), T.target = m, T.relatedTarget = E, S = T), E = S, v && p)
            t: {
              for (T = F0, r = v, s = p, m = 0, S = r; S; S = T(S))
                m++;
              S = 0;
              for (var _ = s; _; _ = T(_))
                S++;
              for (; 0 < m - S; )
                r = T(r), m--;
              for (; 0 < S - m; )
                s = T(s), S--;
              for (; m--; ) {
                if (r === s || s !== null && r === s.alternate) {
                  T = r;
                  break t;
                }
                r = T(r), s = T(s);
              }
              T = null;
            }
          else T = null;
          v !== null && Ks(
            y,
            h,
            v,
            T,
            !1
          ), p !== null && E !== null && Ks(
            y,
            E,
            p,
            T,
            !0
          );
        }
      }
      l: {
        if (h = o ? za(o) : window, v = h.nodeName && h.nodeName.toLowerCase(), v === "select" || v === "input" && h.type === "file")
          var O = ns;
        else if (us(h))
          if (dr)
            O = f0;
          else {
            O = i0;
            var z = n0;
          }
        else
          v = h.nodeName, !v || v.toLowerCase() !== "input" || h.type !== "checkbox" && h.type !== "radio" ? o && Xc(o.elementType) && (O = ns) : O = c0;
        if (O && (O = O(l, o))) {
          rr(
            y,
            O,
            e,
            d
          );
          break l;
        }
        z && z(l, h, o), l === "focusout" && o && h.type === "number" && o.memoizedProps.value != null && wi(h, "number", h.value);
      }
      switch (z = o ? za(o) : window, l) {
        case "focusin":
          (us(z) || z.contentEditable === "true") && (Ue = z, Ji = o, Na = null);
          break;
        case "focusout":
          Na = Ji = Ue = null;
          break;
        case "mousedown":
          ki = !0;
          break;
        case "contextmenu":
        case "mouseup":
        case "dragend":
          ki = !1, os(y, e, d);
          break;
        case "selectionchange":
          if (o0) break;
        case "keydown":
        case "keyup":
          os(y, e, d);
      }
      var x;
      if (Vc)
        l: {
          switch (l) {
            case "compositionstart":
              var j = "onCompositionStart";
              break l;
            case "compositionend":
              j = "onCompositionEnd";
              break l;
            case "compositionupdate":
              j = "onCompositionUpdate";
              break l;
          }
          j = void 0;
        }
      else
        je ? sr(l, e) && (j = "onCompositionEnd") : l === "keydown" && e.keyCode === 229 && (j = "onCompositionStart");
      j && (fr && e.locale !== "ko" && (je || j !== "onCompositionStart" ? j === "onCompositionEnd" && je && (x = cr()) : (Ht = d, Zc = "value" in Ht ? Ht.value : Ht.textContent, je = !0)), z = Sn(o, j), 0 < z.length && (j = new ls(
        j,
        l,
        null,
        e,
        d
      ), y.push({ event: j, listeners: z }), x ? j.data = x : (x = or(e), x !== null && (j.data = x)))), (x = l0 ? t0(l, e) : e0(l, e)) && (j = Sn(o, "onBeforeInput"), 0 < j.length && (z = new ls(
        "onBeforeInput",
        "beforeinput",
        null,
        e,
        d
      ), y.push({
        event: z,
        listeners: j
      }), z.data = x)), J0(
        y,
        l,
        o,
        e,
        d
      );
    }
    im(y, t);
  });
}
function Fa(l, t, e) {
  return {
    instance: l,
    listener: t,
    currentTarget: e
  };
}
function Sn(l, t) {
  for (var e = t + "Capture", a = []; l !== null; ) {
    var u = l, n = u.stateNode;
    if (u = u.tag, u !== 5 && u !== 26 && u !== 27 || n === null || (u = La(l, e), u != null && a.unshift(
      Fa(l, u, n)
    ), u = La(l, t), u != null && a.push(
      Fa(l, u, n)
    )), l.tag === 3) return a;
    l = l.return;
  }
  return [];
}
function F0(l) {
  if (l === null) return null;
  do
    l = l.return;
  while (l && l.tag !== 5 && l.tag !== 27);
  return l || null;
}
function Ks(l, t, e, a, u) {
  for (var n = t._reactName, i = []; e !== null && e !== a; ) {
    var c = e, f = c.alternate, o = c.stateNode;
    if (c = c.tag, f !== null && f === a) break;
    c !== 5 && c !== 26 && c !== 27 || o === null || (f = o, u ? (o = La(e, n), o != null && i.unshift(
      Fa(e, o, f)
    )) : u || (o = La(e, n), o != null && i.push(
      Fa(e, o, f)
    ))), e = e.return;
  }
  i.length !== 0 && l.push({ event: t, listeners: i });
}
var I0 = /\r\n?/g, P0 = /\u0000|\uFFFD/g;
function Js(l) {
  return (typeof l == "string" ? l : "" + l).replace(I0, `
`).replace(P0, "");
}
function fm(l, t) {
  return t = Js(t), Js(l) === t;
}
function K(l, t, e, a, u, n) {
  switch (e) {
    case "children":
      typeof a == "string" ? t === "body" || t === "textarea" && a === "" || ke(l, a) : (typeof a == "number" || typeof a == "bigint") && t !== "body" && ke(l, "" + a);
      break;
    case "className":
      Tu(l, "class", a);
      break;
    case "tabIndex":
      Tu(l, "tabindex", a);
      break;
    case "dir":
    case "role":
    case "viewBox":
    case "width":
    case "height":
      Tu(l, e, a);
      break;
    case "style":
      nr(l, a, n);
      break;
    case "data":
      if (t !== "object") {
        Tu(l, "data", a);
        break;
      }
    case "src":
    case "href":
      if (a === "" && (t !== "a" || e !== "href")) {
        l.removeAttribute(e);
        break;
      }
      if (a == null || typeof a == "function" || typeof a == "symbol" || typeof a == "boolean") {
        l.removeAttribute(e);
        break;
      }
      a = Ru("" + a), l.setAttribute(e, a);
      break;
    case "action":
    case "formAction":
      if (typeof a == "function") {
        l.setAttribute(
          e,
          "javascript:throw new Error('A React form was unexpectedly submitted. If you called form.submit() manually, consider using form.requestSubmit() instead. If you\\'re trying to use event.stopPropagation() in a submit event handler, consider also calling event.preventDefault().')"
        );
        break;
      } else
        typeof n == "function" && (e === "formAction" ? (t !== "input" && K(l, t, "name", u.name, u, null), K(
          l,
          t,
          "formEncType",
          u.formEncType,
          u,
          null
        ), K(
          l,
          t,
          "formMethod",
          u.formMethod,
          u,
          null
        ), K(
          l,
          t,
          "formTarget",
          u.formTarget,
          u,
          null
        )) : (K(l, t, "encType", u.encType, u, null), K(l, t, "method", u.method, u, null), K(l, t, "target", u.target, u, null)));
      if (a == null || typeof a == "symbol" || typeof a == "boolean") {
        l.removeAttribute(e);
        break;
      }
      a = Ru("" + a), l.setAttribute(e, a);
      break;
    case "onClick":
      a != null && (l.onclick = yt);
      break;
    case "onScroll":
      a != null && Y("scroll", l);
      break;
    case "onScrollEnd":
      a != null && Y("scrollend", l);
      break;
    case "dangerouslySetInnerHTML":
      if (a != null) {
        if (typeof a != "object" || !("__html" in a))
          throw Error(b(61));
        if (e = a.__html, e != null) {
          if (u.children != null) throw Error(b(60));
          l.innerHTML = e;
        }
      }
      break;
    case "multiple":
      l.multiple = a && typeof a != "function" && typeof a != "symbol";
      break;
    case "muted":
      l.muted = a && typeof a != "function" && typeof a != "symbol";
      break;
    case "suppressContentEditableWarning":
    case "suppressHydrationWarning":
    case "defaultValue":
    case "defaultChecked":
    case "innerHTML":
    case "ref":
      break;
    case "autoFocus":
      break;
    case "xlinkHref":
      if (a == null || typeof a == "function" || typeof a == "boolean" || typeof a == "symbol") {
        l.removeAttribute("xlink:href");
        break;
      }
      e = Ru("" + a), l.setAttributeNS(
        "http://www.w3.org/1999/xlink",
        "xlink:href",
        e
      );
      break;
    case "contentEditable":
    case "spellCheck":
    case "draggable":
    case "value":
    case "autoReverse":
    case "externalResourcesRequired":
    case "focusable":
    case "preserveAlpha":
      a != null && typeof a != "function" && typeof a != "symbol" ? l.setAttribute(e, "" + a) : l.removeAttribute(e);
      break;
    case "inert":
    case "allowFullScreen":
    case "async":
    case "autoPlay":
    case "controls":
    case "default":
    case "defer":
    case "disabled":
    case "disablePictureInPicture":
    case "disableRemotePlayback":
    case "formNoValidate":
    case "hidden":
    case "loop":
    case "noModule":
    case "noValidate":
    case "open":
    case "playsInline":
    case "readOnly":
    case "required":
    case "reversed":
    case "scoped":
    case "seamless":
    case "itemScope":
      a && typeof a != "function" && typeof a != "symbol" ? l.setAttribute(e, "") : l.removeAttribute(e);
      break;
    case "capture":
    case "download":
      a === !0 ? l.setAttribute(e, "") : a !== !1 && a != null && typeof a != "function" && typeof a != "symbol" ? l.setAttribute(e, a) : l.removeAttribute(e);
      break;
    case "cols":
    case "rows":
    case "size":
    case "span":
      a != null && typeof a != "function" && typeof a != "symbol" && !isNaN(a) && 1 <= a ? l.setAttribute(e, a) : l.removeAttribute(e);
      break;
    case "rowSpan":
    case "start":
      a == null || typeof a == "function" || typeof a == "symbol" || isNaN(a) ? l.removeAttribute(e) : l.setAttribute(e, a);
      break;
    case "popover":
      Y("beforetoggle", l), Y("toggle", l), Hu(l, "popover", a);
      break;
    case "xlinkActuate":
      ct(
        l,
        "http://www.w3.org/1999/xlink",
        "xlink:actuate",
        a
      );
      break;
    case "xlinkArcrole":
      ct(
        l,
        "http://www.w3.org/1999/xlink",
        "xlink:arcrole",
        a
      );
      break;
    case "xlinkRole":
      ct(
        l,
        "http://www.w3.org/1999/xlink",
        "xlink:role",
        a
      );
      break;
    case "xlinkShow":
      ct(
        l,
        "http://www.w3.org/1999/xlink",
        "xlink:show",
        a
      );
      break;
    case "xlinkTitle":
      ct(
        l,
        "http://www.w3.org/1999/xlink",
        "xlink:title",
        a
      );
      break;
    case "xlinkType":
      ct(
        l,
        "http://www.w3.org/1999/xlink",
        "xlink:type",
        a
      );
      break;
    case "xmlBase":
      ct(
        l,
        "http://www.w3.org/XML/1998/namespace",
        "xml:base",
        a
      );
      break;
    case "xmlLang":
      ct(
        l,
        "http://www.w3.org/XML/1998/namespace",
        "xml:lang",
        a
      );
      break;
    case "xmlSpace":
      ct(
        l,
        "http://www.w3.org/XML/1998/namespace",
        "xml:space",
        a
      );
      break;
    case "is":
      Hu(l, "is", a);
      break;
    case "innerText":
    case "textContent":
      break;
    default:
      (!(2 < e.length) || e[0] !== "o" && e[0] !== "O" || e[1] !== "n" && e[1] !== "N") && (e = Oh.get(e) || e, Hu(l, e, a));
  }
}
function Sc(l, t, e, a, u, n) {
  switch (e) {
    case "style":
      nr(l, a, n);
      break;
    case "dangerouslySetInnerHTML":
      if (a != null) {
        if (typeof a != "object" || !("__html" in a))
          throw Error(b(61));
        if (e = a.__html, e != null) {
          if (u.children != null) throw Error(b(60));
          l.innerHTML = e;
        }
      }
      break;
    case "children":
      typeof a == "string" ? ke(l, a) : (typeof a == "number" || typeof a == "bigint") && ke(l, "" + a);
      break;
    case "onScroll":
      a != null && Y("scroll", l);
      break;
    case "onScrollEnd":
      a != null && Y("scrollend", l);
      break;
    case "onClick":
      a != null && (l.onclick = yt);
      break;
    case "suppressContentEditableWarning":
    case "suppressHydrationWarning":
    case "innerHTML":
    case "ref":
      break;
    case "innerText":
    case "textContent":
      break;
    default:
      if (!Po.hasOwnProperty(e))
        l: {
          if (e[0] === "o" && e[1] === "n" && (u = e.endsWith("Capture"), t = e.slice(2, u ? e.length - 7 : void 0), n = l[Nl] || null, n = n != null ? n[e] : null, typeof n == "function" && l.removeEventListener(t, n, u), typeof a == "function")) {
            typeof n != "function" && n !== null && (e in l ? l[e] = null : l.hasAttribute(e) && l.removeAttribute(e)), l.addEventListener(t, a, u);
            break l;
          }
          e in l ? l[e] = a : a === !0 ? l.setAttribute(e, "") : Hu(l, e, a);
        }
  }
}
function _l(l, t, e) {
  switch (t) {
    case "div":
    case "span":
    case "svg":
    case "path":
    case "a":
    case "g":
    case "p":
    case "li":
      break;
    case "img":
      Y("error", l), Y("load", l);
      var a = !1, u = !1, n;
      for (n in e)
        if (e.hasOwnProperty(n)) {
          var i = e[n];
          if (i != null)
            switch (n) {
              case "src":
                a = !0;
                break;
              case "srcSet":
                u = !0;
                break;
              case "children":
              case "dangerouslySetInnerHTML":
                throw Error(b(137, t));
              default:
                K(l, t, n, i, e, null);
            }
        }
      u && K(l, t, "srcSet", e.srcSet, e, null), a && K(l, t, "src", e.src, e, null);
      return;
    case "input":
      Y("invalid", l);
      var c = n = i = u = null, f = null, o = null;
      for (a in e)
        if (e.hasOwnProperty(a)) {
          var d = e[a];
          if (d != null)
            switch (a) {
              case "name":
                u = d;
                break;
              case "type":
                i = d;
                break;
              case "checked":
                f = d;
                break;
              case "defaultChecked":
                o = d;
                break;
              case "value":
                n = d;
                break;
              case "defaultValue":
                c = d;
                break;
              case "children":
              case "dangerouslySetInnerHTML":
                if (d != null)
                  throw Error(b(137, t));
                break;
              default:
                K(l, t, a, d, e, null);
            }
        }
      er(
        l,
        n,
        c,
        f,
        o,
        i,
        u,
        !1
      );
      return;
    case "select":
      Y("invalid", l), a = i = n = null;
      for (u in e)
        if (e.hasOwnProperty(u) && (c = e[u], c != null))
          switch (u) {
            case "value":
              n = c;
              break;
            case "defaultValue":
              i = c;
              break;
            case "multiple":
              a = c;
            default:
              K(l, t, u, c, e, null);
          }
      t = n, e = i, l.multiple = !!a, t != null ? Qe(l, !!a, t, !1) : e != null && Qe(l, !!a, e, !0);
      return;
    case "textarea":
      Y("invalid", l), n = u = a = null;
      for (i in e)
        if (e.hasOwnProperty(i) && (c = e[i], c != null))
          switch (i) {
            case "value":
              a = c;
              break;
            case "defaultValue":
              u = c;
              break;
            case "children":
              n = c;
              break;
            case "dangerouslySetInnerHTML":
              if (c != null) throw Error(b(91));
              break;
            default:
              K(l, t, i, c, e, null);
          }
      ur(l, a, u, n);
      return;
    case "option":
      for (f in e)
        if (e.hasOwnProperty(f) && (a = e[f], a != null))
          switch (f) {
            case "selected":
              l.selected = a && typeof a != "function" && typeof a != "symbol";
              break;
            default:
              K(l, t, f, a, e, null);
          }
      return;
    case "dialog":
      Y("beforetoggle", l), Y("toggle", l), Y("cancel", l), Y("close", l);
      break;
    case "iframe":
    case "object":
      Y("load", l);
      break;
    case "video":
    case "audio":
      for (a = 0; a < Wa.length; a++)
        Y(Wa[a], l);
      break;
    case "image":
      Y("error", l), Y("load", l);
      break;
    case "details":
      Y("toggle", l);
      break;
    case "embed":
    case "source":
    case "link":
      Y("error", l), Y("load", l);
    case "area":
    case "base":
    case "br":
    case "col":
    case "hr":
    case "keygen":
    case "meta":
    case "param":
    case "track":
    case "wbr":
    case "menuitem":
      for (o in e)
        if (e.hasOwnProperty(o) && (a = e[o], a != null))
          switch (o) {
            case "children":
            case "dangerouslySetInnerHTML":
              throw Error(b(137, t));
            default:
              K(l, t, o, a, e, null);
          }
      return;
    default:
      if (Xc(t)) {
        for (d in e)
          e.hasOwnProperty(d) && (a = e[d], a !== void 0 && Sc(
            l,
            t,
            d,
            a,
            e,
            void 0
          ));
        return;
      }
  }
  for (c in e)
    e.hasOwnProperty(c) && (a = e[c], a != null && K(l, t, c, a, e, null));
}
function ly(l, t, e, a) {
  switch (t) {
    case "div":
    case "span":
    case "svg":
    case "path":
    case "a":
    case "g":
    case "p":
    case "li":
      break;
    case "input":
      var u = null, n = null, i = null, c = null, f = null, o = null, d = null;
      for (v in e) {
        var y = e[v];
        if (e.hasOwnProperty(v) && y != null)
          switch (v) {
            case "checked":
              break;
            case "value":
              break;
            case "defaultValue":
              f = y;
            default:
              a.hasOwnProperty(v) || K(l, t, v, null, a, y);
          }
      }
      for (var h in a) {
        var v = a[h];
        if (y = e[h], a.hasOwnProperty(h) && (v != null || y != null))
          switch (h) {
            case "type":
              n = v;
              break;
            case "name":
              u = v;
              break;
            case "checked":
              o = v;
              break;
            case "defaultChecked":
              d = v;
              break;
            case "value":
              i = v;
              break;
            case "defaultValue":
              c = v;
              break;
            case "children":
            case "dangerouslySetInnerHTML":
              if (v != null)
                throw Error(b(137, t));
              break;
            default:
              v !== y && K(
                l,
                t,
                h,
                v,
                a,
                y
              );
          }
      }
      Zi(
        l,
        i,
        c,
        f,
        o,
        d,
        n,
        u
      );
      return;
    case "select":
      v = i = c = h = null;
      for (n in e)
        if (f = e[n], e.hasOwnProperty(n) && f != null)
          switch (n) {
            case "value":
              break;
            case "multiple":
              v = f;
            default:
              a.hasOwnProperty(n) || K(
                l,
                t,
                n,
                null,
                a,
                f
              );
          }
      for (u in a)
        if (n = a[u], f = e[u], a.hasOwnProperty(u) && (n != null || f != null))
          switch (u) {
            case "value":
              h = n;
              break;
            case "defaultValue":
              c = n;
              break;
            case "multiple":
              i = n;
            default:
              n !== f && K(
                l,
                t,
                u,
                n,
                a,
                f
              );
          }
      t = c, e = i, a = v, h != null ? Qe(l, !!e, h, !1) : !!a != !!e && (t != null ? Qe(l, !!e, t, !0) : Qe(l, !!e, e ? [] : "", !1));
      return;
    case "textarea":
      v = h = null;
      for (c in e)
        if (u = e[c], e.hasOwnProperty(c) && u != null && !a.hasOwnProperty(c))
          switch (c) {
            case "value":
              break;
            case "children":
              break;
            default:
              K(l, t, c, null, a, u);
          }
      for (i in a)
        if (u = a[i], n = e[i], a.hasOwnProperty(i) && (u != null || n != null))
          switch (i) {
            case "value":
              h = u;
              break;
            case "defaultValue":
              v = u;
              break;
            case "children":
              break;
            case "dangerouslySetInnerHTML":
              if (u != null) throw Error(b(91));
              break;
            default:
              u !== n && K(l, t, i, u, a, n);
          }
      ar(l, h, v);
      return;
    case "option":
      for (var p in e)
        if (h = e[p], e.hasOwnProperty(p) && h != null && !a.hasOwnProperty(p))
          switch (p) {
            case "selected":
              l.selected = !1;
              break;
            default:
              K(
                l,
                t,
                p,
                null,
                a,
                h
              );
          }
      for (f in a)
        if (h = a[f], v = e[f], a.hasOwnProperty(f) && h !== v && (h != null || v != null))
          switch (f) {
            case "selected":
              l.selected = h && typeof h != "function" && typeof h != "symbol";
              break;
            default:
              K(
                l,
                t,
                f,
                h,
                a,
                v
              );
          }
      return;
    case "img":
    case "link":
    case "area":
    case "base":
    case "br":
    case "col":
    case "embed":
    case "hr":
    case "keygen":
    case "meta":
    case "param":
    case "source":
    case "track":
    case "wbr":
    case "menuitem":
      for (var T in e)
        h = e[T], e.hasOwnProperty(T) && h != null && !a.hasOwnProperty(T) && K(l, t, T, null, a, h);
      for (o in a)
        if (h = a[o], v = e[o], a.hasOwnProperty(o) && h !== v && (h != null || v != null))
          switch (o) {
            case "children":
            case "dangerouslySetInnerHTML":
              if (h != null)
                throw Error(b(137, t));
              break;
            default:
              K(
                l,
                t,
                o,
                h,
                a,
                v
              );
          }
      return;
    default:
      if (Xc(t)) {
        for (var E in e)
          h = e[E], e.hasOwnProperty(E) && h !== void 0 && !a.hasOwnProperty(E) && Sc(
            l,
            t,
            E,
            void 0,
            a,
            h
          );
        for (d in a)
          h = a[d], v = e[d], !a.hasOwnProperty(d) || h === v || h === void 0 && v === void 0 || Sc(
            l,
            t,
            d,
            h,
            a,
            v
          );
        return;
      }
  }
  for (var r in e)
    h = e[r], e.hasOwnProperty(r) && h != null && !a.hasOwnProperty(r) && K(l, t, r, null, a, h);
  for (y in a)
    h = a[y], v = e[y], !a.hasOwnProperty(y) || h === v || h == null && v == null || K(l, t, y, h, a, v);
}
function ks(l) {
  switch (l) {
    case "css":
    case "script":
    case "font":
    case "img":
    case "image":
    case "input":
    case "link":
      return !0;
    default:
      return !1;
  }
}
function ty() {
  if (typeof performance.getEntriesByType == "function") {
    for (var l = 0, t = 0, e = performance.getEntriesByType("resource"), a = 0; a < e.length; a++) {
      var u = e[a], n = u.transferSize, i = u.initiatorType, c = u.duration;
      if (n && c && ks(i)) {
        for (i = 0, c = u.responseEnd, a += 1; a < e.length; a++) {
          var f = e[a], o = f.startTime;
          if (o > c) break;
          var d = f.transferSize, y = f.initiatorType;
          d && ks(y) && (f = f.responseEnd, i += d * (f < c ? 1 : (c - o) / (f - o)));
        }
        if (--a, t += 8 * (n + i) / (u.duration / 1e3), l++, 10 < l) break;
      }
    }
    if (0 < l) return t / l / 1e6;
  }
  return navigator.connection && (l = navigator.connection.downlink, typeof l == "number") ? l : 5;
}
var bc = null, _c = null;
function bn(l) {
  return l.nodeType === 9 ? l : l.ownerDocument;
}
function $s(l) {
  switch (l) {
    case "http://www.w3.org/2000/svg":
      return 1;
    case "http://www.w3.org/1998/Math/MathML":
      return 2;
    default:
      return 0;
  }
}
function sm(l, t) {
  if (l === 0)
    switch (t) {
      case "svg":
        return 1;
      case "math":
        return 2;
      default:
        return 0;
    }
  return l === 1 && t === "foreignObject" ? 0 : l;
}
function Ec(l, t) {
  return l === "textarea" || l === "noscript" || typeof t.children == "string" || typeof t.children == "number" || typeof t.children == "bigint" || typeof t.dangerouslySetInnerHTML == "object" && t.dangerouslySetInnerHTML !== null && t.dangerouslySetInnerHTML.__html != null;
}
var xi = null;
function ey() {
  var l = window.event;
  return l && l.type === "popstate" ? l === xi ? !1 : (xi = l, !0) : (xi = null, !1);
}
var om = typeof setTimeout == "function" ? setTimeout : void 0, ay = typeof clearTimeout == "function" ? clearTimeout : void 0, Ws = typeof Promise == "function" ? Promise : void 0, uy = typeof queueMicrotask == "function" ? queueMicrotask : typeof Ws < "u" ? function(l) {
  return Ws.resolve(null).then(l).catch(ny);
} : om;
function ny(l) {
  setTimeout(function() {
    throw l;
  });
}
function It(l) {
  return l === "head";
}
function Fs(l, t) {
  var e = t, a = 0;
  do {
    var u = e.nextSibling;
    if (l.removeChild(e), u && u.nodeType === 8)
      if (e = u.data, e === "/$" || e === "/&") {
        if (a === 0) {
          l.removeChild(u), ea(t);
          return;
        }
        a--;
      } else if (e === "$" || e === "$?" || e === "$~" || e === "$!" || e === "&")
        a++;
      else if (e === "html")
        Qa(l.ownerDocument.documentElement);
      else if (e === "head") {
        e = l.ownerDocument.head, Qa(e);
        for (var n = e.firstChild; n; ) {
          var i = n.nextSibling, c = n.nodeName;
          n[nu] || c === "SCRIPT" || c === "STYLE" || c === "LINK" && n.rel.toLowerCase() === "stylesheet" || e.removeChild(n), n = i;
        }
      } else
        e === "body" && Qa(l.ownerDocument.body);
    e = u;
  } while (e);
  ea(t);
}
function Is(l, t) {
  var e = l;
  l = 0;
  do {
    var a = e.nextSibling;
    if (e.nodeType === 1 ? t ? (e._stashedDisplay = e.style.display, e.style.display = "none") : (e.style.display = e._stashedDisplay || "", e.getAttribute("style") === "" && e.removeAttribute("style")) : e.nodeType === 3 && (t ? (e._stashedText = e.nodeValue, e.nodeValue = "") : e.nodeValue = e._stashedText || ""), a && a.nodeType === 8)
      if (e = a.data, e === "/$") {
        if (l === 0) break;
        l--;
      } else
        e !== "$" && e !== "$?" && e !== "$~" && e !== "$!" || l++;
    e = a;
  } while (e);
}
function pc(l) {
  var t = l.firstChild;
  for (t && t.nodeType === 10 && (t = t.nextSibling); t; ) {
    var e = t;
    switch (t = t.nextSibling, e.nodeName) {
      case "HTML":
      case "HEAD":
      case "BODY":
        pc(e), Qc(e);
        continue;
      case "SCRIPT":
      case "STYLE":
        continue;
      case "LINK":
        if (e.rel.toLowerCase() === "stylesheet") continue;
    }
    l.removeChild(e);
  }
}
function iy(l, t, e, a) {
  for (; l.nodeType === 1; ) {
    var u = e;
    if (l.nodeName.toLowerCase() !== t.toLowerCase()) {
      if (!a && (l.nodeName !== "INPUT" || l.type !== "hidden"))
        break;
    } else if (a) {
      if (!l[nu])
        switch (t) {
          case "meta":
            if (!l.hasAttribute("itemprop")) break;
            return l;
          case "link":
            if (n = l.getAttribute("rel"), n === "stylesheet" && l.hasAttribute("data-precedence"))
              break;
            if (n !== u.rel || l.getAttribute("href") !== (u.href == null || u.href === "" ? null : u.href) || l.getAttribute("crossorigin") !== (u.crossOrigin == null ? null : u.crossOrigin) || l.getAttribute("title") !== (u.title == null ? null : u.title))
              break;
            return l;
          case "style":
            if (l.hasAttribute("data-precedence")) break;
            return l;
          case "script":
            if (n = l.getAttribute("src"), (n !== (u.src == null ? null : u.src) || l.getAttribute("type") !== (u.type == null ? null : u.type) || l.getAttribute("crossorigin") !== (u.crossOrigin == null ? null : u.crossOrigin)) && n && l.hasAttribute("async") && !l.hasAttribute("itemprop"))
              break;
            return l;
          default:
            return l;
        }
    } else if (t === "input" && l.type === "hidden") {
      var n = u.name == null ? null : "" + u.name;
      if (u.type === "hidden" && l.getAttribute("name") === n)
        return l;
    } else return l;
    if (l = Wl(l.nextSibling), l === null) break;
  }
  return null;
}
function cy(l, t, e) {
  if (t === "") return null;
  for (; l.nodeType !== 3; )
    if ((l.nodeType !== 1 || l.nodeName !== "INPUT" || l.type !== "hidden") && !e || (l = Wl(l.nextSibling), l === null)) return null;
  return l;
}
function rm(l, t) {
  for (; l.nodeType !== 8; )
    if ((l.nodeType !== 1 || l.nodeName !== "INPUT" || l.type !== "hidden") && !t || (l = Wl(l.nextSibling), l === null)) return null;
  return l;
}
function Tc(l) {
  return l.data === "$?" || l.data === "$~";
}
function zc(l) {
  return l.data === "$!" || l.data === "$?" && l.ownerDocument.readyState !== "loading";
}
function fy(l, t) {
  var e = l.ownerDocument;
  if (l.data === "$~") l._reactRetry = t;
  else if (l.data !== "$?" || e.readyState !== "loading")
    t();
  else {
    var a = function() {
      t(), e.removeEventListener("DOMContentLoaded", a);
    };
    e.addEventListener("DOMContentLoaded", a), l._reactRetry = a;
  }
}
function Wl(l) {
  for (; l != null; l = l.nextSibling) {
    var t = l.nodeType;
    if (t === 1 || t === 3) break;
    if (t === 8) {
      if (t = l.data, t === "$" || t === "$!" || t === "$?" || t === "$~" || t === "&" || t === "F!" || t === "F")
        break;
      if (t === "/$" || t === "/&") return null;
    }
  }
  return l;
}
var Ac = null;
function Ps(l) {
  l = l.nextSibling;
  for (var t = 0; l; ) {
    if (l.nodeType === 8) {
      var e = l.data;
      if (e === "/$" || e === "/&") {
        if (t === 0)
          return Wl(l.nextSibling);
        t--;
      } else
        e !== "$" && e !== "$!" && e !== "$?" && e !== "$~" && e !== "&" || t++;
    }
    l = l.nextSibling;
  }
  return null;
}
function lo(l) {
  l = l.previousSibling;
  for (var t = 0; l; ) {
    if (l.nodeType === 8) {
      var e = l.data;
      if (e === "$" || e === "$!" || e === "$?" || e === "$~" || e === "&") {
        if (t === 0) return l;
        t--;
      } else e !== "/$" && e !== "/&" || t++;
    }
    l = l.previousSibling;
  }
  return null;
}
function dm(l, t, e) {
  switch (t = bn(e), l) {
    case "html":
      if (l = t.documentElement, !l) throw Error(b(452));
      return l;
    case "head":
      if (l = t.head, !l) throw Error(b(453));
      return l;
    case "body":
      if (l = t.body, !l) throw Error(b(454));
      return l;
    default:
      throw Error(b(451));
  }
}
function Qa(l) {
  for (var t = l.attributes; t.length; )
    l.removeAttributeNode(t[0]);
  Qc(l);
}
var Fl = /* @__PURE__ */ new Map(), to = /* @__PURE__ */ new Set();
function _n(l) {
  return typeof l.getRootNode == "function" ? l.getRootNode() : l.nodeType === 9 ? l : l.ownerDocument;
}
var At = Z.d;
Z.d = {
  f: sy,
  r: oy,
  D: ry,
  C: dy,
  L: my,
  m: hy,
  X: vy,
  S: yy,
  M: gy
};
function sy() {
  var l = At.f(), t = Xn();
  return l || t;
}
function oy(l) {
  var t = ia(l);
  t !== null && t.tag === 5 && t.type === "form" ? nd(t) : At.r(l);
}
var oa = typeof document > "u" ? null : document;
function mm(l, t, e) {
  var a = oa;
  if (a && typeof t == "string" && t) {
    var u = Kl(t);
    u = 'link[rel="' + l + '"][href="' + u + '"]', typeof e == "string" && (u += '[crossorigin="' + e + '"]'), to.has(u) || (to.add(u), l = { rel: l, crossOrigin: e, href: t }, a.querySelector(u) === null && (t = a.createElement("link"), _l(t, "link", l), hl(t), a.head.appendChild(t)));
  }
}
function ry(l) {
  At.D(l), mm("dns-prefetch", l, null);
}
function dy(l, t) {
  At.C(l, t), mm("preconnect", l, t);
}
function my(l, t, e) {
  At.L(l, t, e);
  var a = oa;
  if (a && l && t) {
    var u = 'link[rel="preload"][as="' + Kl(t) + '"]';
    t === "image" && e && e.imageSrcSet ? (u += '[imagesrcset="' + Kl(
      e.imageSrcSet
    ) + '"]', typeof e.imageSizes == "string" && (u += '[imagesizes="' + Kl(
      e.imageSizes
    ) + '"]')) : u += '[href="' + Kl(l) + '"]';
    var n = u;
    switch (t) {
      case "style":
        n = ta(l);
        break;
      case "script":
        n = ra(l);
    }
    Fl.has(n) || (l = P(
      {
        rel: "preload",
        href: t === "image" && e && e.imageSrcSet ? void 0 : l,
        as: t
      },
      e
    ), Fl.set(n, l), a.querySelector(u) !== null || t === "style" && a.querySelector(ru(n)) || t === "script" && a.querySelector(du(n)) || (t = a.createElement("link"), _l(t, "link", l), hl(t), a.head.appendChild(t)));
  }
}
function hy(l, t) {
  At.m(l, t);
  var e = oa;
  if (e && l) {
    var a = t && typeof t.as == "string" ? t.as : "script", u = 'link[rel="modulepreload"][as="' + Kl(a) + '"][href="' + Kl(l) + '"]', n = u;
    switch (a) {
      case "audioworklet":
      case "paintworklet":
      case "serviceworker":
      case "sharedworker":
      case "worker":
      case "script":
        n = ra(l);
    }
    if (!Fl.has(n) && (l = P({ rel: "modulepreload", href: l }, t), Fl.set(n, l), e.querySelector(u) === null)) {
      switch (a) {
        case "audioworklet":
        case "paintworklet":
        case "serviceworker":
        case "sharedworker":
        case "worker":
        case "script":
          if (e.querySelector(du(n)))
            return;
      }
      a = e.createElement("link"), _l(a, "link", l), hl(a), e.head.appendChild(a);
    }
  }
}
function yy(l, t, e) {
  At.S(l, t, e);
  var a = oa;
  if (a && l) {
    var u = Ge(a).hoistableStyles, n = ta(l);
    t = t || "default";
    var i = u.get(n);
    if (!i) {
      var c = { loading: 0, preload: null };
      if (i = a.querySelector(
        ru(n)
      ))
        c.loading = 5;
      else {
        l = P(
          { rel: "stylesheet", href: l, "data-precedence": t },
          e
        ), (e = Fl.get(n)) && Mf(l, e);
        var f = i = a.createElement("link");
        hl(f), _l(f, "link", l), f._p = new Promise(function(o, d) {
          f.onload = o, f.onerror = d;
        }), f.addEventListener("load", function() {
          c.loading |= 1;
        }), f.addEventListener("error", function() {
          c.loading |= 2;
        }), c.loading |= 4, Vu(i, t, a);
      }
      i = {
        type: "stylesheet",
        instance: i,
        count: 1,
        state: c
      }, u.set(n, i);
    }
  }
}
function vy(l, t) {
  At.X(l, t);
  var e = oa;
  if (e && l) {
    var a = Ge(e).hoistableScripts, u = ra(l), n = a.get(u);
    n || (n = e.querySelector(du(u)), n || (l = P({ src: l, async: !0 }, t), (t = Fl.get(u)) && Of(l, t), n = e.createElement("script"), hl(n), _l(n, "link", l), e.head.appendChild(n)), n = {
      type: "script",
      instance: n,
      count: 1,
      state: null
    }, a.set(u, n));
  }
}
function gy(l, t) {
  At.M(l, t);
  var e = oa;
  if (e && l) {
    var a = Ge(e).hoistableScripts, u = ra(l), n = a.get(u);
    n || (n = e.querySelector(du(u)), n || (l = P({ src: l, async: !0, type: "module" }, t), (t = Fl.get(u)) && Of(l, t), n = e.createElement("script"), hl(n), _l(n, "link", l), e.head.appendChild(n)), n = {
      type: "script",
      instance: n,
      count: 1,
      state: null
    }, a.set(u, n));
  }
}
function eo(l, t, e, a) {
  var u = (u = qt.current) ? _n(u) : null;
  if (!u) throw Error(b(446));
  switch (l) {
    case "meta":
    case "title":
      return null;
    case "style":
      return typeof e.precedence == "string" && typeof e.href == "string" ? (t = ta(e.href), e = Ge(
        u
      ).hoistableStyles, a = e.get(t), a || (a = {
        type: "style",
        instance: null,
        count: 0,
        state: null
      }, e.set(t, a)), a) : { type: "void", instance: null, count: 0, state: null };
    case "link":
      if (e.rel === "stylesheet" && typeof e.href == "string" && typeof e.precedence == "string") {
        l = ta(e.href);
        var n = Ge(
          u
        ).hoistableStyles, i = n.get(l);
        if (i || (u = u.ownerDocument || u, i = {
          type: "stylesheet",
          instance: null,
          count: 0,
          state: { loading: 0, preload: null }
        }, n.set(l, i), (n = u.querySelector(
          ru(l)
        )) && !n._p && (i.instance = n, i.state.loading = 5), Fl.has(l) || (e = {
          rel: "preload",
          as: "style",
          href: e.href,
          crossOrigin: e.crossOrigin,
          integrity: e.integrity,
          media: e.media,
          hrefLang: e.hrefLang,
          referrerPolicy: e.referrerPolicy
        }, Fl.set(l, e), n || Sy(
          u,
          l,
          e,
          i.state
        ))), t && a === null)
          throw Error(b(528, ""));
        return i;
      }
      if (t && a !== null)
        throw Error(b(529, ""));
      return null;
    case "script":
      return t = e.async, e = e.src, typeof e == "string" && t && typeof t != "function" && typeof t != "symbol" ? (t = ra(e), e = Ge(
        u
      ).hoistableScripts, a = e.get(t), a || (a = {
        type: "script",
        instance: null,
        count: 0,
        state: null
      }, e.set(t, a)), a) : { type: "void", instance: null, count: 0, state: null };
    default:
      throw Error(b(444, l));
  }
}
function ta(l) {
  return 'href="' + Kl(l) + '"';
}
function ru(l) {
  return 'link[rel="stylesheet"][' + l + "]";
}
function hm(l) {
  return P({}, l, {
    "data-precedence": l.precedence,
    precedence: null
  });
}
function Sy(l, t, e, a) {
  l.querySelector('link[rel="preload"][as="style"][' + t + "]") ? a.loading = 1 : (t = l.createElement("link"), a.preload = t, t.addEventListener("load", function() {
    return a.loading |= 1;
  }), t.addEventListener("error", function() {
    return a.loading |= 2;
  }), _l(t, "link", e), hl(t), l.head.appendChild(t));
}
function ra(l) {
  return '[src="' + Kl(l) + '"]';
}
function du(l) {
  return "script[async]" + l;
}
function ao(l, t, e) {
  if (t.count++, t.instance === null)
    switch (t.type) {
      case "style":
        var a = l.querySelector(
          'style[data-href~="' + Kl(e.href) + '"]'
        );
        if (a)
          return t.instance = a, hl(a), a;
        var u = P({}, e, {
          "data-href": e.href,
          "data-precedence": e.precedence,
          href: null,
          precedence: null
        });
        return a = (l.ownerDocument || l).createElement(
          "style"
        ), hl(a), _l(a, "style", u), Vu(a, e.precedence, l), t.instance = a;
      case "stylesheet":
        u = ta(e.href);
        var n = l.querySelector(
          ru(u)
        );
        if (n)
          return t.state.loading |= 4, t.instance = n, hl(n), n;
        a = hm(e), (u = Fl.get(u)) && Mf(a, u), n = (l.ownerDocument || l).createElement("link"), hl(n);
        var i = n;
        return i._p = new Promise(function(c, f) {
          i.onload = c, i.onerror = f;
        }), _l(n, "link", a), t.state.loading |= 4, Vu(n, e.precedence, l), t.instance = n;
      case "script":
        return n = ra(e.src), (u = l.querySelector(
          du(n)
        )) ? (t.instance = u, hl(u), u) : (a = e, (u = Fl.get(n)) && (a = P({}, e), Of(a, u)), l = l.ownerDocument || l, u = l.createElement("script"), hl(u), _l(u, "link", a), l.head.appendChild(u), t.instance = u);
      case "void":
        return null;
      default:
        throw Error(b(443, t.type));
    }
  else
    t.type === "stylesheet" && !(t.state.loading & 4) && (a = t.instance, t.state.loading |= 4, Vu(a, e.precedence, l));
  return t.instance;
}
function Vu(l, t, e) {
  for (var a = e.querySelectorAll(
    'link[rel="stylesheet"][data-precedence],style[data-precedence]'
  ), u = a.length ? a[a.length - 1] : null, n = u, i = 0; i < a.length; i++) {
    var c = a[i];
    if (c.dataset.precedence === t) n = c;
    else if (n !== u) break;
  }
  n ? n.parentNode.insertBefore(l, n.nextSibling) : (t = e.nodeType === 9 ? e.head : e, t.insertBefore(l, t.firstChild));
}
function Mf(l, t) {
  l.crossOrigin == null && (l.crossOrigin = t.crossOrigin), l.referrerPolicy == null && (l.referrerPolicy = t.referrerPolicy), l.title == null && (l.title = t.title);
}
function Of(l, t) {
  l.crossOrigin == null && (l.crossOrigin = t.crossOrigin), l.referrerPolicy == null && (l.referrerPolicy = t.referrerPolicy), l.integrity == null && (l.integrity = t.integrity);
}
var Ku = null;
function uo(l, t, e) {
  if (Ku === null) {
    var a = /* @__PURE__ */ new Map(), u = Ku = /* @__PURE__ */ new Map();
    u.set(e, a);
  } else
    u = Ku, a = u.get(e), a || (a = /* @__PURE__ */ new Map(), u.set(e, a));
  if (a.has(l)) return a;
  for (a.set(l, null), e = e.getElementsByTagName(l), u = 0; u < e.length; u++) {
    var n = e[u];
    if (!(n[nu] || n[gl] || l === "link" && n.getAttribute("rel") === "stylesheet") && n.namespaceURI !== "http://www.w3.org/2000/svg") {
      var i = n.getAttribute(t) || "";
      i = l + i;
      var c = a.get(i);
      c ? c.push(n) : a.set(i, [n]);
    }
  }
  return a;
}
function no(l, t, e) {
  l = l.ownerDocument || l, l.head.insertBefore(
    e,
    t === "title" ? l.querySelector("head > title") : null
  );
}
function by(l, t, e) {
  if (e === 1 || t.itemProp != null) return !1;
  switch (l) {
    case "meta":
    case "title":
      return !0;
    case "style":
      if (typeof t.precedence != "string" || typeof t.href != "string" || t.href === "")
        break;
      return !0;
    case "link":
      if (typeof t.rel != "string" || typeof t.href != "string" || t.href === "" || t.onLoad || t.onError)
        break;
      switch (t.rel) {
        case "stylesheet":
          return l = t.disabled, typeof t.precedence == "string" && l == null;
        default:
          return !0;
      }
    case "script":
      if (t.async && typeof t.async != "function" && typeof t.async != "symbol" && !t.onLoad && !t.onError && t.src && typeof t.src == "string")
        return !0;
  }
  return !1;
}
function ym(l) {
  return !(l.type === "stylesheet" && !(l.state.loading & 3));
}
function _y(l, t, e, a) {
  if (e.type === "stylesheet" && (typeof a.media != "string" || matchMedia(a.media).matches !== !1) && !(e.state.loading & 4)) {
    if (e.instance === null) {
      var u = ta(a.href), n = t.querySelector(
        ru(u)
      );
      if (n) {
        t = n._p, t !== null && typeof t == "object" && typeof t.then == "function" && (l.count++, l = En.bind(l), t.then(l, l)), e.state.loading |= 4, e.instance = n, hl(n);
        return;
      }
      n = t.ownerDocument || t, a = hm(a), (u = Fl.get(u)) && Mf(a, u), n = n.createElement("link"), hl(n);
      var i = n;
      i._p = new Promise(function(c, f) {
        i.onload = c, i.onerror = f;
      }), _l(n, "link", a), e.instance = n;
    }
    l.stylesheets === null && (l.stylesheets = /* @__PURE__ */ new Map()), l.stylesheets.set(e, t), (t = e.state.preload) && !(e.state.loading & 3) && (l.count++, e = En.bind(l), t.addEventListener("load", e), t.addEventListener("error", e));
  }
}
var Ni = 0;
function Ey(l, t) {
  return l.stylesheets && l.count === 0 && Ju(l, l.stylesheets), 0 < l.count || 0 < l.imgCount ? function(e) {
    var a = setTimeout(function() {
      if (l.stylesheets && Ju(l, l.stylesheets), l.unsuspend) {
        var n = l.unsuspend;
        l.unsuspend = null, n();
      }
    }, 6e4 + t);
    0 < l.imgBytes && Ni === 0 && (Ni = 62500 * ty());
    var u = setTimeout(
      function() {
        if (l.waitingForImages = !1, l.count === 0 && (l.stylesheets && Ju(l, l.stylesheets), l.unsuspend)) {
          var n = l.unsuspend;
          l.unsuspend = null, n();
        }
      },
      (l.imgBytes > Ni ? 50 : 800) + t
    );
    return l.unsuspend = e, function() {
      l.unsuspend = null, clearTimeout(a), clearTimeout(u);
    };
  } : null;
}
function En() {
  if (this.count--, this.count === 0 && (this.imgCount === 0 || !this.waitingForImages)) {
    if (this.stylesheets) Ju(this, this.stylesheets);
    else if (this.unsuspend) {
      var l = this.unsuspend;
      this.unsuspend = null, l();
    }
  }
}
var pn = null;
function Ju(l, t) {
  l.stylesheets = null, l.unsuspend !== null && (l.count++, pn = /* @__PURE__ */ new Map(), t.forEach(py, l), pn = null, En.call(l));
}
function py(l, t) {
  if (!(t.state.loading & 4)) {
    var e = pn.get(l);
    if (e) var a = e.get(null);
    else {
      e = /* @__PURE__ */ new Map(), pn.set(l, e);
      for (var u = l.querySelectorAll(
        "link[data-precedence],style[data-precedence]"
      ), n = 0; n < u.length; n++) {
        var i = u[n];
        (i.nodeName === "LINK" || i.getAttribute("media") !== "not all") && (e.set(i.dataset.precedence, i), a = i);
      }
      a && e.set(null, a);
    }
    u = t.instance, i = u.getAttribute("data-precedence"), n = e.get(i) || a, n === a && e.set(null, u), e.set(i, u), this.count++, a = En.bind(this), u.addEventListener("load", a), u.addEventListener("error", a), n ? n.parentNode.insertBefore(u, n.nextSibling) : (l = l.nodeType === 9 ? l.head : l, l.insertBefore(u, l.firstChild)), t.state.loading |= 4;
  }
}
var Ia = {
  $$typeof: ht,
  Provider: null,
  Consumer: null,
  _currentValue: ne,
  _currentValue2: ne,
  _threadCount: 0
};
function Ty(l, t, e, a, u, n, i, c, f) {
  this.tag = 1, this.containerInfo = l, this.pingCache = this.current = this.pendingChildren = null, this.timeoutHandle = -1, this.callbackNode = this.next = this.pendingContext = this.context = this.cancelPendingCommit = null, this.callbackPriority = 0, this.expirationTimes = Pn(-1), this.entangledLanes = this.shellSuspendCounter = this.errorRecoveryDisabledLanes = this.expiredLanes = this.warmLanes = this.pingedLanes = this.suspendedLanes = this.pendingLanes = 0, this.entanglements = Pn(0), this.hiddenUpdates = Pn(null), this.identifierPrefix = a, this.onUncaughtError = u, this.onCaughtError = n, this.onRecoverableError = i, this.pooledCache = null, this.pooledCacheLanes = 0, this.formState = f, this.incompleteTransitions = /* @__PURE__ */ new Map();
}
function vm(l, t, e, a, u, n, i, c, f, o, d, y) {
  return l = new Ty(
    l,
    t,
    e,
    i,
    f,
    o,
    d,
    y,
    c
  ), t = 1, n === !0 && (t |= 24), n = Hl(3, null, null, t), l.current = n, n.stateNode = l, t = Ic(), t.refCount++, l.pooledCache = t, t.refCount++, n.memoizedState = {
    element: a,
    isDehydrated: e,
    cache: t
  }, tf(n), l;
}
function gm(l) {
  return l ? (l = Re, l) : Re;
}
function Sm(l, t, e, a, u, n) {
  u = gm(u), a.context === null ? a.context = u : a.pendingContext = u, a = Qt(t), a.payload = { element: e }, n = n === void 0 ? null : n, n !== null && (a.callback = n), e = Xt(l, a, t), e !== null && (xl(e, l, t), Ua(e, l, t));
}
function io(l, t) {
  if (l = l.memoizedState, l !== null && l.dehydrated !== null) {
    var e = l.retryLane;
    l.retryLane = e !== 0 && e < t ? e : t;
  }
}
function Df(l, t) {
  io(l, t), (l = l.alternate) && io(l, t);
}
function bm(l) {
  if (l.tag === 13 || l.tag === 31) {
    var t = Se(l, 67108864);
    t !== null && xl(t, l, 67108864), Df(l, 67108864);
  }
}
function co(l) {
  if (l.tag === 13 || l.tag === 31) {
    var t = Gl();
    t = qc(t);
    var e = Se(l, t);
    e !== null && xl(e, l, t), Df(l, t);
  }
}
var Tn = !0;
function zy(l, t, e, a) {
  var u = C.T;
  C.T = null;
  var n = Z.p;
  try {
    Z.p = 2, xf(l, t, e, a);
  } finally {
    Z.p = n, C.T = u;
  }
}
function Ay(l, t, e, a) {
  var u = C.T;
  C.T = null;
  var n = Z.p;
  try {
    Z.p = 8, xf(l, t, e, a);
  } finally {
    Z.p = n, C.T = u;
  }
}
function xf(l, t, e, a) {
  if (Tn) {
    var u = Mc(a);
    if (u === null)
      Di(
        l,
        t,
        a,
        zn,
        e
      ), fo(l, a);
    else if (Oy(
      u,
      l,
      t,
      e,
      a
    ))
      a.stopPropagation();
    else if (fo(l, a), t & 4 && -1 < My.indexOf(l)) {
      for (; u !== null; ) {
        var n = ia(u);
        if (n !== null)
          switch (n.tag) {
            case 3:
              if (n = n.stateNode, n.current.memoizedState.isDehydrated) {
                var i = ee(n.pendingLanes);
                if (i !== 0) {
                  var c = n;
                  for (c.pendingLanes |= 2, c.entangledLanes |= 2; i; ) {
                    var f = 1 << 31 - ql(i);
                    c.entanglements[1] |= f, i &= ~f;
                  }
                  it(n), !(L & 6) && (mn = Bl() + 500, ou(0));
                }
              }
              break;
            case 31:
            case 13:
              c = Se(n, 2), c !== null && xl(c, n, 2), Xn(), Df(n, 2);
          }
        if (n = Mc(a), n === null && Di(
          l,
          t,
          a,
          zn,
          e
        ), n === u) break;
        u = n;
      }
      u !== null && a.stopPropagation();
    } else
      Di(
        l,
        t,
        a,
        null,
        e
      );
  }
}
function Mc(l) {
  return l = Lc(l), Nf(l);
}
var zn = null;
function Nf(l) {
  if (zn = null, l = xe(l), l !== null) {
    var t = tu(l);
    if (t === null) l = null;
    else {
      var e = t.tag;
      if (e === 13) {
        if (l = Go(t), l !== null) return l;
        l = null;
      } else if (e === 31) {
        if (l = Qo(t), l !== null) return l;
        l = null;
      } else if (e === 3) {
        if (t.stateNode.current.memoizedState.isDehydrated)
          return t.tag === 3 ? t.stateNode.containerInfo : null;
        l = null;
      } else t !== l && (l = null);
    }
  }
  return zn = l, null;
}
function _m(l) {
  switch (l) {
    case "beforetoggle":
    case "cancel":
    case "click":
    case "close":
    case "contextmenu":
    case "copy":
    case "cut":
    case "auxclick":
    case "dblclick":
    case "dragend":
    case "dragstart":
    case "drop":
    case "focusin":
    case "focusout":
    case "input":
    case "invalid":
    case "keydown":
    case "keypress":
    case "keyup":
    case "mousedown":
    case "mouseup":
    case "paste":
    case "pause":
    case "play":
    case "pointercancel":
    case "pointerdown":
    case "pointerup":
    case "ratechange":
    case "reset":
    case "resize":
    case "seeked":
    case "submit":
    case "toggle":
    case "touchcancel":
    case "touchend":
    case "touchstart":
    case "volumechange":
    case "change":
    case "selectionchange":
    case "textInput":
    case "compositionstart":
    case "compositionend":
    case "compositionupdate":
    case "beforeblur":
    case "afterblur":
    case "beforeinput":
    case "blur":
    case "fullscreenchange":
    case "focus":
    case "hashchange":
    case "popstate":
    case "select":
    case "selectstart":
      return 2;
    case "drag":
    case "dragenter":
    case "dragexit":
    case "dragleave":
    case "dragover":
    case "mousemove":
    case "mouseout":
    case "mouseover":
    case "pointermove":
    case "pointerout":
    case "pointerover":
    case "scroll":
    case "touchmove":
    case "wheel":
    case "mouseenter":
    case "mouseleave":
    case "pointerenter":
    case "pointerleave":
      return 8;
    case "message":
      switch (rh()) {
        case wo:
          return 2;
        case Vo:
          return 8;
        case Iu:
        case dh:
          return 32;
        case Ko:
          return 268435456;
        default:
          return 32;
      }
    default:
      return 32;
  }
}
var Oc = !1, wt = null, Vt = null, Kt = null, Pa = /* @__PURE__ */ new Map(), lu = /* @__PURE__ */ new Map(), Ut = [], My = "mousedown mouseup touchcancel touchend touchstart auxclick dblclick pointercancel pointerdown pointerup dragend dragstart drop compositionend compositionstart keydown keypress keyup input textInput copy cut paste click change contextmenu reset".split(
  " "
);
function fo(l, t) {
  switch (l) {
    case "focusin":
    case "focusout":
      wt = null;
      break;
    case "dragenter":
    case "dragleave":
      Vt = null;
      break;
    case "mouseover":
    case "mouseout":
      Kt = null;
      break;
    case "pointerover":
    case "pointerout":
      Pa.delete(t.pointerId);
      break;
    case "gotpointercapture":
    case "lostpointercapture":
      lu.delete(t.pointerId);
  }
}
function _a(l, t, e, a, u, n) {
  return l === null || l.nativeEvent !== n ? (l = {
    blockedOn: t,
    domEventName: e,
    eventSystemFlags: a,
    nativeEvent: n,
    targetContainers: [u]
  }, t !== null && (t = ia(t), t !== null && bm(t)), l) : (l.eventSystemFlags |= a, t = l.targetContainers, u !== null && t.indexOf(u) === -1 && t.push(u), l);
}
function Oy(l, t, e, a, u) {
  switch (t) {
    case "focusin":
      return wt = _a(
        wt,
        l,
        t,
        e,
        a,
        u
      ), !0;
    case "dragenter":
      return Vt = _a(
        Vt,
        l,
        t,
        e,
        a,
        u
      ), !0;
    case "mouseover":
      return Kt = _a(
        Kt,
        l,
        t,
        e,
        a,
        u
      ), !0;
    case "pointerover":
      var n = u.pointerId;
      return Pa.set(
        n,
        _a(
          Pa.get(n) || null,
          l,
          t,
          e,
          a,
          u
        )
      ), !0;
    case "gotpointercapture":
      return n = u.pointerId, lu.set(
        n,
        _a(
          lu.get(n) || null,
          l,
          t,
          e,
          a,
          u
        )
      ), !0;
  }
  return !1;
}
function Em(l) {
  var t = xe(l.target);
  if (t !== null) {
    var e = tu(t);
    if (e !== null) {
      if (t = e.tag, t === 13) {
        if (t = Go(e), t !== null) {
          l.blockedOn = t, Kf(l.priority, function() {
            co(e);
          });
          return;
        }
      } else if (t === 31) {
        if (t = Qo(e), t !== null) {
          l.blockedOn = t, Kf(l.priority, function() {
            co(e);
          });
          return;
        }
      } else if (t === 3 && e.stateNode.current.memoizedState.isDehydrated) {
        l.blockedOn = e.tag === 3 ? e.stateNode.containerInfo : null;
        return;
      }
    }
  }
  l.blockedOn = null;
}
function ku(l) {
  if (l.blockedOn !== null) return !1;
  for (var t = l.targetContainers; 0 < t.length; ) {
    var e = Mc(l.nativeEvent);
    if (e === null) {
      e = l.nativeEvent;
      var a = new e.constructor(
        e.type,
        e
      );
      Vi = a, e.target.dispatchEvent(a), Vi = null;
    } else
      return t = ia(e), t !== null && bm(t), l.blockedOn = e, !1;
    t.shift();
  }
  return !0;
}
function so(l, t, e) {
  ku(l) && e.delete(t);
}
function Dy() {
  Oc = !1, wt !== null && ku(wt) && (wt = null), Vt !== null && ku(Vt) && (Vt = null), Kt !== null && ku(Kt) && (Kt = null), Pa.forEach(so), lu.forEach(so);
}
function ju(l, t) {
  l.blockedOn === t && (l.blockedOn = null, Oc || (Oc = !0, ol.unstable_scheduleCallback(
    ol.unstable_NormalPriority,
    Dy
  )));
}
var Uu = null;
function oo(l) {
  Uu !== l && (Uu = l, ol.unstable_scheduleCallback(
    ol.unstable_NormalPriority,
    function() {
      Uu === l && (Uu = null);
      for (var t = 0; t < l.length; t += 3) {
        var e = l[t], a = l[t + 1], u = l[t + 2];
        if (typeof a != "function") {
          if (Nf(a || e) === null)
            continue;
          break;
        }
        var n = ia(e);
        n !== null && (l.splice(t, 3), t -= 3, cc(
          n,
          {
            pending: !0,
            data: u,
            method: e.method,
            action: a
          },
          a,
          u
        ));
      }
    }
  ));
}
function ea(l) {
  function t(f) {
    return ju(f, l);
  }
  wt !== null && ju(wt, l), Vt !== null && ju(Vt, l), Kt !== null && ju(Kt, l), Pa.forEach(t), lu.forEach(t);
  for (var e = 0; e < Ut.length; e++) {
    var a = Ut[e];
    a.blockedOn === l && (a.blockedOn = null);
  }
  for (; 0 < Ut.length && (e = Ut[0], e.blockedOn === null); )
    Em(e), e.blockedOn === null && Ut.shift();
  if (e = (l.ownerDocument || l).$$reactFormReplay, e != null)
    for (a = 0; a < e.length; a += 3) {
      var u = e[a], n = e[a + 1], i = u[Nl] || null;
      if (typeof n == "function")
        i || oo(e);
      else if (i) {
        var c = null;
        if (n && n.hasAttribute("formAction")) {
          if (u = n, i = n[Nl] || null)
            c = i.formAction;
          else if (Nf(u) !== null) continue;
        } else c = i.action;
        typeof c == "function" ? e[a + 1] = c : (e.splice(a, 3), a -= 3), oo(e);
      }
    }
}
function pm() {
  function l(n) {
    n.canIntercept && n.info === "react-transition" && n.intercept({
      handler: function() {
        return new Promise(function(i) {
          return u = i;
        });
      },
      focusReset: "manual",
      scroll: "manual"
    });
  }
  function t() {
    u !== null && (u(), u = null), a || setTimeout(e, 20);
  }
  function e() {
    if (!a && !navigation.transition) {
      var n = navigation.currentEntry;
      n && n.url != null && navigation.navigate(n.url, {
        state: n.getState(),
        info: "react-transition",
        history: "replace"
      });
    }
  }
  if (typeof navigation == "object") {
    var a = !1, u = null;
    return navigation.addEventListener("navigate", l), navigation.addEventListener("navigatesuccess", t), navigation.addEventListener("navigateerror", t), setTimeout(e, 100), function() {
      a = !0, navigation.removeEventListener("navigate", l), navigation.removeEventListener("navigatesuccess", t), navigation.removeEventListener("navigateerror", t), u !== null && (u(), u = null);
    };
  }
}
function jf(l) {
  this._internalRoot = l;
}
wn.prototype.render = jf.prototype.render = function(l) {
  var t = this._internalRoot;
  if (t === null) throw Error(b(409));
  var e = t.current, a = Gl();
  Sm(e, a, l, t, null, null);
};
wn.prototype.unmount = jf.prototype.unmount = function() {
  var l = this._internalRoot;
  if (l !== null) {
    this._internalRoot = null;
    var t = l.containerInfo;
    Sm(l.current, 2, null, l, null, null), Xn(), t[na] = null;
  }
};
function wn(l) {
  this._internalRoot = l;
}
wn.prototype.unstable_scheduleHydration = function(l) {
  if (l) {
    var t = Fo();
    l = { blockedOn: null, target: l, priority: t };
    for (var e = 0; e < Ut.length && t !== 0 && t < Ut[e].priority; e++) ;
    Ut.splice(e, 0, l), e === 0 && Em(l);
  }
};
var ro = Yo.version;
if (ro !== "19.2.7")
  throw Error(
    b(
      527,
      ro,
      "19.2.7"
    )
  );
Z.findDOMNode = function(l) {
  var t = l._reactInternals;
  if (t === void 0)
    throw typeof l.render == "function" ? Error(b(188)) : (l = Object.keys(l).join(","), Error(b(268, l)));
  return l = uh(t), l = l !== null ? Xo(l) : null, l = l === null ? null : l.stateNode, l;
};
var xy = {
  bundleType: 0,
  version: "19.2.7",
  rendererPackageName: "react-dom",
  currentDispatcherRef: C,
  reconcilerVersion: "19.2.7"
};
if (typeof __REACT_DEVTOOLS_GLOBAL_HOOK__ < "u") {
  var Cu = __REACT_DEVTOOLS_GLOBAL_HOOK__;
  if (!Cu.isDisabled && Cu.supportsFiber)
    try {
      eu = Cu.inject(
        xy
      ), Yl = Cu;
    } catch {
    }
}
On.createRoot = function(l, t) {
  if (!qo(l)) throw Error(b(299));
  var e = !1, a = "", u = md, n = hd, i = yd;
  return t != null && (t.unstable_strictMode === !0 && (e = !0), t.identifierPrefix !== void 0 && (a = t.identifierPrefix), t.onUncaughtError !== void 0 && (u = t.onUncaughtError), t.onCaughtError !== void 0 && (n = t.onCaughtError), t.onRecoverableError !== void 0 && (i = t.onRecoverableError)), t = vm(
    l,
    1,
    !1,
    null,
    null,
    e,
    a,
    null,
    u,
    n,
    i,
    pm
  ), l[na] = t.current, Af(l), new jf(t);
};
On.hydrateRoot = function(l, t, e) {
  if (!qo(l)) throw Error(b(299));
  var a = !1, u = "", n = md, i = hd, c = yd, f = null;
  return e != null && (e.unstable_strictMode === !0 && (a = !0), e.identifierPrefix !== void 0 && (u = e.identifierPrefix), e.onUncaughtError !== void 0 && (n = e.onUncaughtError), e.onCaughtError !== void 0 && (i = e.onCaughtError), e.onRecoverableError !== void 0 && (c = e.onRecoverableError), e.formState !== void 0 && (f = e.formState)), t = vm(
    l,
    1,
    !0,
    t,
    e ?? null,
    a,
    u,
    f,
    n,
    i,
    c,
    pm
  ), t.context = gm(null), e = t.current, a = Gl(), a = qc(a), u = Qt(a), u.callback = null, Xt(e, u, a), e = a, t.current.lanes = e, uu(t, e), it(t), l[na] = t.current, Af(l), new wn(t);
};
On.version = "19.2.7";
function Tm() {
  if (!(typeof __REACT_DEVTOOLS_GLOBAL_HOOK__ > "u" || typeof __REACT_DEVTOOLS_GLOBAL_HOOK__.checkDCE != "function"))
    try {
      __REACT_DEVTOOLS_GLOBAL_HOOK__.checkDCE(Tm);
    } catch (l) {
      console.error(l);
    }
}
Tm(), To.exports = On;
var Ny = To.exports;
function Uf(l) {
  return l.items.filter((t) => !t.hidden);
}
function zm(l, t) {
  const e = t.trim().toLowerCase();
  return l.map((a) => ({
    ...a,
    items: Uf(a).filter((u) => !e || u.label.toLowerCase().includes(e) || (u.keywords ?? []).some((n) => n.toLowerCase().includes(e)))
  })).filter((a) => a.items.length > 0);
}
function jy(l) {
  var t;
  return (t = l.map(Uf).find((e) => e.length > 0)) == null ? void 0 : t[0];
}
function Am(l) {
  return l.flatMap(Uf);
}
function Uy(l, t, e, a) {
  if (a <= 0)
    return null;
  if (t === "Home")
    return 0;
  if (t === "End")
    return a - 1;
  const u = "ArrowDown", n = "ArrowUp";
  return t === u ? (e + 1) % a : t === n ? (e - 1 + a) % a : null;
}
const Mm = ["system", "light", "dark"];
function Cy() {
  try {
    return typeof window > "u" ? null : window.localStorage;
  } catch {
    return null;
  }
}
function Hy(l, t) {
  return l === "system" ? t ? "dark" : "light" : l;
}
function Ry(l) {
  const t = () => l.storage !== void 0 ? l.storage : Cy(), e = () => l.root !== void 0 ? l.root : typeof document > "u" ? null : document.documentElement, a = /* @__PURE__ */ new Set();
  let u = null;
  function n(d) {
    const y = e();
    y && (d === "system" ? y.removeAttribute("data-theme") : y.setAttribute("data-theme", d));
  }
  function i() {
    var d;
    if (l.qaOverride === "light" || l.qaOverride === "dark")
      return l.qaOverride;
    try {
      const y = (d = t()) == null ? void 0 : d.getItem(l.storageKey);
      if (y === "light" || y === "dark")
        return y;
    } catch {
    }
    return "system";
  }
  function c() {
    return u === null && (u = i()), u;
  }
  function f(d) {
    var y, h;
    try {
      d === "system" ? (y = t()) == null || y.removeItem(l.storageKey) : (h = t()) == null || h.setItem(l.storageKey, d);
    } catch {
    }
    u = d, n(d);
    for (const v of a)
      v(d);
  }
  function o(d) {
    return a.add(d), () => a.delete(d);
  }
  return { read: c, apply: n, save: f, subscribe: o };
}
function By(l, t) {
  let e = 0, a = -1, u = 0;
  for (let n = 0; n < t.length && e < l.length; n++)
    l[e] === t[n] ? (a < 0 && (a = n), e++) : a >= 0 && u++;
  return e === l.length ? 40 - Math.min(30, a + u) : null;
}
function Yy(l, t, e = [], a = "") {
  const u = l.trim().toLowerCase();
  if (!u)
    return 0;
  const n = t.toLowerCase();
  if (n === u)
    return 200;
  if (n.startsWith(u))
    return 160 - Math.min(20, n.length - u.length);
  const i = n.split(/[\s/_.:›-]+/).findIndex((f) => f.startsWith(u));
  if (i >= 0)
    return 120 - i;
  const c = n.indexOf(u);
  return c >= 0 ? 100 - Math.min(40, c) : e.some((f) => f.toLowerCase().includes(u)) ? 70 : a && a.toLowerCase().includes(u) ? 60 : By(u, n);
}
function qy(l, t, e = 50) {
  const a = [];
  return l.forEach((u, n) => {
    const i = Yy(t, u.label, u.keywords, u.hint);
    i !== null && a.push({ item: u, score: i, index: n });
  }), t.trim() && a.sort((u, n) => n.score - u.score || u.index - n.index), a.slice(0, e).map((u) => u.item);
}
function mo(l, t = "Commands") {
  const e = [], a = /* @__PURE__ */ new Map();
  for (const u of l) {
    const n = u.group ?? t;
    a.has(n) || (a.set(n, []), e.push(n)), a.get(n).push(u);
  }
  return e.map((u) => ({ name: u, items: a.get(u) }));
}
const ho = { active: 0, pointer: null, keyboardDriving: !1 };
function Gy(l, t, e) {
  var a;
  if (l.keyboardDriving) {
    if (l.pointer === null)
      return { ...l, pointer: e };
    if (l.pointer.x === e.x && l.pointer.y === e.y)
      return l;
  } else if (((a = l.pointer) == null ? void 0 : a.x) === e.x && l.pointer.y === e.y)
    return l;
  return { active: t, pointer: e, keyboardDriving: !1 };
}
function Qy(l, t, e) {
  const a = t === "down" ? 1 : -1, u = e === 0 ? 0 : (l.active + a + e) % e;
  return { ...l, active: u, keyboardDriving: !0 };
}
function yo({ x: l, y: t, width: e, estimatedHeight: a, viewportWidth: u, viewportHeight: n, gutter: i = 8 }) {
  const c = Math.max(0, Math.min(i, Math.floor(u / 2), Math.floor(n / 2))), f = Math.max(0, u - c * 2), o = Math.max(0, n - c * 2), d = Math.min(e, f), y = Math.min(a, o), h = Math.max(c, u - c - d), v = Math.max(c, n - c - y);
  return {
    left: Math.max(c, Math.min(l, h)),
    top: Math.max(c, Math.min(t, v)),
    width: d,
    maxWidth: f,
    maxHeight: o
  };
}
const Xy = { info: 3e3, success: 3e3, warning: 5e3, error: 6e3 };
function Om(l = {}) {
  let t = 0;
  const e = /* @__PURE__ */ new Set(), a = { ...Xy, ...l.ttl };
  return {
    push(u, n = {}) {
      const i = typeof n == "string" ? { kind: n } : n, c = i.kind ?? "info", f = { id: ++t, message: u, kind: c, action: i.action, ttl: i.ttl ?? a[c] };
      for (const o of e)
        o({ type: "push", toast: f });
      return f.id;
    },
    dismiss(u) {
      for (const n of e)
        n({ type: "dismiss", id: u });
    },
    subscribe(u) {
      return e.add(u), () => e.delete(u);
    }
  };
}
const Ly = Om();
function Zy() {
  return window.__TAURI_OS_PLUGIN_INTERNALS__.platform;
}
function vo(l) {
  return l === "Win32" || l === "win32" ? "windows" : l === "macos" || l === "ios" || l === "windows" || l === "linux" ? l : "unknown";
}
let Ea;
function da() {
  if (globalThis.__RIGHTKIT_QA_PLATFORM__ !== void 0)
    return vo(globalThis.__RIGHTKIT_QA_PLATFORM__);
  if (Ea)
    return Ea;
  try {
    Ea = vo(Zy());
  } catch {
    Ea = "unknown";
  }
  return Ea;
}
const wy = {
  mod: "⌘",
  ctrl: "⌃",
  alt: "⌥",
  shift: "⇧",
  fn: "fn",
  win: "⌘",
  space: "Space",
  enter: "↩",
  esc: "Esc"
}, Vy = {
  mod: "Ctrl",
  ctrl: "Ctrl",
  rctrl: "Right Ctrl",
  alt: "Alt",
  ralt: "Right Alt",
  shift: "Shift",
  fn: "Fn",
  win: "Win",
  space: "Space",
  enter: "Enter",
  esc: "Esc"
}, Ky = {
  mod: "Touch",
  ctrl: "Touch",
  alt: "Touch",
  shift: "Touch",
  fn: "Touch",
  win: "Touch",
  space: "Record",
  enter: "Send",
  esc: "Cancel",
  record: "Record",
  send: "Send",
  cancel: "Cancel"
}, Jy = {
  mod: "Mod",
  ctrl: "Ctrl",
  rctrl: "Right Ctrl",
  alt: "Alt",
  ralt: "Right Alt",
  shift: "Shift",
  fn: "Fn",
  win: "System",
  space: "Space",
  enter: "Enter",
  esc: "Esc"
};
function ky(l, t = da()) {
  const e = t === "macos" ? wy : t === "ios" ? Ky : t === "windows" || t === "linux" ? Vy : Jy;
  return l.split("+").map((a) => a.trim().toLowerCase()).filter(Boolean).map((a) => e[a] ?? (a.length === 1 ? a.toUpperCase() : a.replace(/^\w/, (u) => u.toUpperCase())));
}
function $y(l = da()) {
  return {
    desktopWindowControls: l === "macos" || l === "windows" || l === "linux",
    nativeTrafficLights: l === "macos",
    customCaptionButtons: l === "windows" || l === "linux",
    touchFirst: l === "ios"
  };
}
const Wy = {
  minimize: "Minimize window",
  maximize: "Maximize or restore window",
  close: "Close window",
  enterFullscreen: "Enter fullscreen",
  exitFullscreen: "Exit fullscreen"
};
function Fy(l = {}) {
  return { ...Wy, ...l };
}
function Iy(l) {
  return {
    minimize: () => l.minimize(),
    toggleMaximize: () => l.toggleMaximize(),
    close: () => l.close()
  };
}
const Dm = { esc: "escape", plus: "+", return: "enter", del: "delete", up: "arrowup", down: "arrowdown", left: "arrowleft", right: "arrowright" };
function Py(l) {
  const t = { mod: !1, ctrl: !1, meta: !1, alt: !1, shift: !1, key: "" };
  for (const e of l.split("+").map((a) => a.trim().toLowerCase()).filter(Boolean))
    e === "mod" ? t.mod = !0 : e === "ctrl" || e === "control" ? t.ctrl = !0 : e === "meta" || e === "cmd" || e === "command" ? t.meta = !0 : e === "alt" || e === "option" ? t.alt = !0 : e === "shift" ? t.shift = !0 : t.key = Dm[e] ?? e;
  return t;
}
function Dc(l) {
  return l.trim().split(/\s+/).filter(Boolean).map(Py);
}
function lv(l) {
  return Dc(l).map((t) => [t.mod && "mod", t.ctrl && "ctrl", t.meta && "meta", t.alt && "alt", t.shift && "shift", t.key].filter(Boolean).join("+")).join(" ");
}
function xm(l) {
  return l.trim().split(/\s+/).filter(Boolean);
}
function tv(l) {
  return {
    key: l.key === " " ? "space" : l.key.toLowerCase(),
    code: l.code ?? "",
    ctrl: l.ctrlKey,
    meta: l.metaKey,
    alt: l.altKey,
    shift: l.shiftKey
  };
}
const ev = /* @__PURE__ */ new Set(["shift", "control", "alt", "meta", "altgraph", "capslock"]);
function av(l, t, e) {
  const a = e === "macos" || e === "ios", u = l.meta || l.mod && a, n = l.ctrl || l.mod && !a;
  if (t.meta !== u || t.ctrl !== n || t.alt !== l.alt)
    return !1;
  let c = (Dm[t.key] ?? t.key) === l.key;
  return !c && l.alt && l.key.length === 1 && (c = t.code === `Key${l.key.toUpperCase()}` || t.code === `Digit${l.key}`), c ? l.key.length === 1 && !/[a-z0-9]/.test(l.key) || t.shift === l.shift : !1;
}
function uv(l) {
  const t = l;
  return !t || typeof t.closest != "function" ? !1 : t.closest("input:not([type=checkbox]):not([type=radio]):not([type=button]), textarea, select, [contenteditable=''], [contenteditable='true'], .cm-editor") !== null;
}
function nv(l = {}) {
  const t = l.platform ?? da(), e = l.sequenceTimeout ?? 1e3, a = /* @__PURE__ */ new Map();
  let u = {}, n = [], i = 0;
  const c = /* @__PURE__ */ new Set();
  let f = 0;
  const o = () => {
    f++;
    for (const s of c)
      s();
  }, d = (s) => {
    const m = u[s.def.id];
    return m === null ? [] : m !== void 0 ? [m] : s.defaults;
  };
  function y(s) {
    const m = { def: s, defaults: typeof s.chord == "string" ? [s.chord] : [...s.chord] };
    return a.set(s.id, m), o(), () => {
      a.get(s.id) === m && (a.delete(s.id), o());
    };
  }
  const h = (s, m, S) => {
    if (s.def.when && s.def.when() === !1)
      return !1;
    if (!m)
      return !0;
    if (s.def.allowInInput !== void 0)
      return s.def.allowInInput;
    const _ = Dc(S);
    if (_.length !== 1)
      return !1;
    const O = _[0];
    return O.mod || O.ctrl || O.meta || O.alt;
  };
  function v(s) {
    if (s.isComposing || s.defaultPrevented)
      return !1;
    const m = tv(s);
    if (ev.has(m.key))
      return !1;
    const S = uv(s.target), _ = Date.now();
    n.length && _ - i > e && (n = []);
    for (let O = 0; O < 2; O++) {
      const z = [...n, m];
      let x, j = !1;
      for (const U of a.values())
        for (const B of d(U)) {
          const rl = Dc(B);
          rl.length < z.length || z.every((_e, ma) => av(rl[ma], _e, t)) && h(U, S, B) && (rl.length === z.length ? x ?? (x = U) : j = !0);
        }
      if (x)
        return n = [], x.def.run(s) === !1 ? !1 : (x.def.preventDefault !== !1 && s.preventDefault(), !0);
      if (j)
        return n = z, i = _, !0;
      if (n.length === 0)
        return !1;
      n = [];
    }
    return !1;
  }
  function p(s = window) {
    const m = (S) => void v(S);
    return s.addEventListener("keydown", m), () => s.removeEventListener("keydown", m);
  }
  function T() {
    return [...a.values()].map((s) => ({
      id: s.def.id,
      label: s.def.label,
      group: s.def.group ?? "General",
      chords: d(s),
      defaults: s.defaults,
      rebindable: s.def.rebindable ?? !1,
      hidden: s.def.hidden ?? !1,
      overridden: u[s.def.id] !== void 0
    }));
  }
  function E() {
    const s = [], m = /* @__PURE__ */ new Map();
    for (const S of T())
      S.hidden || S.chords.length === 0 || (m.has(S.group) || (m.set(S.group, []), s.push(S.group)), m.get(S.group).push(S));
    return s.map((S) => ({ title: S, items: m.get(S) }));
  }
  function r() {
    const s = /* @__PURE__ */ new Map();
    for (const m of T())
      for (const S of m.chords) {
        const _ = lv(S);
        s.set(_, [...s.get(_) ?? [], m.id]);
      }
    return [...s].filter(([, m]) => m.length > 1).map(([m, S]) => ({ chord: m, ids: S }));
  }
  return {
    platform: t,
    register: y,
    handle: v,
    attach: p,
    list: T,
    groups: E,
    conflicts: r,
    /** Run a binding by id (native menu, palette, tests) without a key event. */
    invoke(s) {
      const m = a.get(s);
      if (!m || m.def.when && m.def.when() === !1)
        return !1;
      const S = typeof KeyboardEvent == "function" ? new KeyboardEvent("keydown") : {};
      return m.def.run(S) !== !1;
    },
    chordFor: (s) => {
      const m = a.get(s);
      return m ? d(m)[0] : void 0;
    },
    /** `null` unbinds; `undefined` entries restore the default. */
    setOverrides(s) {
      u = { ...s }, o();
    },
    overrides: () => ({ ...u }),
    subscribe(s) {
      return c.add(s), () => c.delete(s);
    },
    version: () => f
  };
}
function iv() {
  const l = /* @__PURE__ */ new Set(), t = /* @__PURE__ */ new Set();
  let e = [];
  const a = () => {
    const u = /* @__PURE__ */ new Map();
    for (const n of l)
      for (const i of n)
        u.set(i.id, i);
    e = [...u.values()].filter((n) => !n.disabled);
    for (const n of t)
      n();
  };
  return {
    /** Later registrations win on id collisions. Returns the unregister function. */
    register(u) {
      return l.add(u), a(), () => {
        l.delete(u), a();
      };
    },
    list: () => e,
    run(u) {
      const n = e.find((i) => i.id === u);
      return n ? (n.run(), !0) : !1;
    },
    subscribe(u) {
      return t.add(u), () => t.delete(u);
    }
  };
}
const cv = 'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';
function mu(l) {
  const t = A.useRef(l);
  return t.current = l, t;
}
function Cf(l, t = !0, e = {}) {
  const { autoFocus: a = !0 } = e, u = mu(t), n = A.useRef(typeof document > "u" ? null : document.activeElement);
  A.useEffect(() => {
    const i = l.current;
    if (!t || !i)
      return;
    const c = n.current, f = () => Array.from(i.querySelectorAll(cv)).filter((d) => !d.hasAttribute("hidden"));
    a && (f()[0] ?? i).focus();
    const o = (d) => {
      if (d.key !== "Tab")
        return;
      const y = f();
      if (!y.length) {
        d.preventDefault();
        return;
      }
      const h = y[0], v = y[y.length - 1], p = document.activeElement;
      d.shiftKey && (p === h || !i.contains(p)) ? (d.preventDefault(), v.focus()) : !d.shiftKey && (p === v || !i.contains(p)) && (d.preventDefault(), h.focus());
    };
    return i.addEventListener("keydown", o), () => {
      i.removeEventListener("keydown", o), queueMicrotask(() => {
        var d;
        (!i.isConnected || !u.current) && ((d = c == null ? void 0 : c.focus) == null || d.call(c));
      });
    };
  }, [l, t, a, u]);
}
function Hf(l, t) {
  const [e, a] = A.useState(!1), u = A.useRef(null), n = mu(l), i = (t == null ? void 0 : t.exitMs) ?? 80, c = A.useRef(!1), f = A.useCallback(() => {
    c.current || (c.current = !0, a(!0), u.current = window.setTimeout(() => {
      var d;
      return (d = n.current) == null ? void 0 : d.call(n);
    }, i));
  }, [i, n]);
  A.useEffect(() => () => {
    u.current != null && window.clearTimeout(u.current);
  }, []);
  const o = (t == null ? void 0 : t.escape) ?? !1;
  return A.useEffect(() => {
    if (!o)
      return;
    const d = (y) => {
      y.key === "Escape" && (y.stopPropagation(), y.preventDefault(), f());
    };
    return window.addEventListener("keydown", d, !0), () => window.removeEventListener("keydown", d, !0);
  }, [o, f]), { closing: e, close: f };
}
function Nm(l) {
  const [t, e] = A.useState(() => typeof window < "u" && window.matchMedia ? window.matchMedia(l).matches : !1);
  return A.useEffect(() => {
    if (typeof window > "u" || !window.matchMedia)
      return;
    const a = window.matchMedia(l), u = () => e(a.matches);
    return u(), a.addEventListener("change", u), () => a.removeEventListener("change", u);
  }, [l]), t;
}
function Vn() {
  const [l] = A.useState(da);
  return l;
}
function hu({ chord: l, platform: t = da(), gap: e = 4, className: a = "rightkit-kbd-chord", keyClassName: u = "rightkit-kbd-key", ariaLabel: n }) {
  const i = ky(l, t);
  return g.jsx("span", { className: a, "aria-label": n ?? i.join(" + "), style: { "--rightkit-kbd-gap": `${e}px` }, children: i.map((c, f) => g.jsx("kbd", { className: u, children: c }, `${c}-${f}`)) });
}
const Rf = (...l) => l.filter(Boolean).join(" "), An = A.forwardRef(function({ variant: t = "secondary", size: e = "md", className: a, type: u = "button", ...n }, i) {
  return g.jsx("button", { ref: i, type: u, className: Rf("rk-btn", `rk-btn--${t}`, e === "sm" && "rk-btn--sm", a), ...n });
}), xc = A.forwardRef(function({ label: t, pressed: e, className: a, type: u = "button", ...n }, i) {
  return g.jsx("button", { ref: i, type: u, "aria-label": t, title: t, "aria-pressed": e, className: Rf("rk-icon-btn", e && "is-open", a), ...n });
}), fv = A.forwardRef(function({ className: t, ...e }, a) {
  return g.jsx("input", { ref: a, className: Rf("rk-input", t), ...e });
});
function sv({ label: l, onClose: t, width: e = 440, align: a = "center", autoFocus: u = !0, role: n = "dialog", dismissOnBackdrop: i = !0, labelledBy: c, describedBy: f, children: o }) {
  const d = A.useRef(null), { closing: y, close: h } = Hf(t, { escape: !0 });
  return Cf(d, !0, { autoFocus: u }), g.jsx("div", { className: `rk-overlay rk-overlay--${a}${y ? " is-closing" : ""}`, onMouseDown: (v) => v.target === v.currentTarget && i && h(), children: g.jsx("div", { ref: d, role: n, "aria-modal": "true", "aria-label": c ? void 0 : l, "aria-labelledby": c, "aria-describedby": f, tabIndex: -1, style: { width: e }, className: `rk-modal${y ? " is-closing" : ""}`, children: o }) });
}
function jm({ title: l, description: t, children: e, actions: a, ...u }) {
  const n = A.useId(), i = A.useId();
  return g.jsxs(sv, { ...u, label: l, labelledBy: n, describedBy: t ? i : void 0, children: [g.jsx("h2", { id: n, className: "rk-modal__title", children: l }), t ? g.jsx("p", { id: i, className: "rk-modal__desc", children: t }) : null, e, a ? g.jsx("div", { className: "rk-modal__actions", children: a }) : null] });
}
function ov({ title: l, description: t, confirmLabel: e = "Confirm", cancelLabel: a = "Cancel", danger: u, onConfirm: n, onCancel: i }) {
  return g.jsx(jm, { title: l, description: t, role: u ? "alertdialog" : "dialog", onClose: i, actions: g.jsxs(g.Fragment, { children: [g.jsx(An, { onClick: i, autoFocus: u, children: a }), g.jsx(An, { variant: u ? "danger" : "primary", onClick: n, autoFocus: !u, children: e })] }), autoFocus: !1 });
}
function rv({ title: l, label: t, initialValue: e = "", placeholder: a, confirmLabel: u = "OK", cancelLabel: n = "Cancel", validate: i, onSubmit: c, onCancel: f }) {
  const [o, d] = A.useState(e), [y, h] = A.useState(null), v = A.useId(), p = (T) => {
    T.preventDefault();
    const E = (i == null ? void 0 : i(o)) ?? null;
    h(E), E || c(o);
  };
  return g.jsx(jm, { title: l, onClose: f, autoFocus: !1, children: g.jsxs("form", { onSubmit: p, className: "rk-modal__form", children: [g.jsx("label", { htmlFor: v, className: "rk-modal__label", children: t }), g.jsx(fv, { id: v, autoFocus: !0, value: o, placeholder: a, "aria-invalid": y ? !0 : void 0, onChange: (T) => d(T.target.value), onFocus: (T) => T.currentTarget.select() }), y ? g.jsx("small", { className: "rk-modal__error", role: "alert", children: y }) : null, g.jsxs("div", { className: "rk-modal__actions", children: [g.jsx(An, { onClick: f, children: n }), g.jsx(An, { variant: "primary", type: "submit", children: u })] })] }) });
}
const go = (l) => l.map((t, e) => t.disabled ? -1 : e).filter((t) => t >= 0);
function dv({ x: l, y: t, items: e, onClose: a, label: u, width: n = 200 }) {
  const i = A.useRef(null), c = A.useRef(`rk-menu-${Math.random().toString(36).slice(2, 8)}`), f = A.useRef(typeof document < "u" ? document.activeElement : null), [o, d] = A.useState(() => go(e)[0] ?? -1), [y, h] = A.useState(() => yo({ x: l, y: t, width: n, estimatedHeight: e.length * 28 + 12, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight }));
  A.useLayoutEffect(() => {
    var r;
    const E = (r = i.current) == null ? void 0 : r.offsetHeight;
    E && h(yo({ x: l, y: t, width: n, estimatedHeight: E, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight }));
  }, [l, t, n, e]);
  const v = A.useCallback(() => {
    var E, r;
    a(), (r = (E = f.current) == null ? void 0 : E.focus) == null || r.call(E);
  }, [a]);
  A.useEffect(() => {
    var r;
    (r = i.current) == null || r.focus({ preventScroll: !0 });
    const E = () => a();
    return window.addEventListener("resize", E), window.addEventListener("blur", E), () => {
      window.removeEventListener("resize", E), window.removeEventListener("blur", E);
    };
  }, [a]), A.useEffect(() => {
    var E, r;
    (r = (E = document.getElementById(`${c.current}-${o}`)) == null ? void 0 : E.scrollIntoView) == null || r.call(E, { block: "nearest" });
  }, [o]);
  const p = (E) => {
    var r, s, m;
    E.disabled || (a(), (s = (r = f.current) == null ? void 0 : r.focus) == null || s.call(r), (m = E.run) == null || m.call(E));
  }, T = (E) => {
    const r = go(e), s = r.indexOf(o);
    E.key === "Escape" ? (E.preventDefault(), E.stopPropagation(), v()) : E.key === "ArrowDown" ? (E.preventDefault(), d(r[(s + 1) % r.length] ?? -1)) : E.key === "ArrowUp" ? (E.preventDefault(), d(r[(s - 1 + r.length) % r.length] ?? -1)) : E.key === "Home" ? (E.preventDefault(), d(r[0] ?? -1)) : E.key === "End" ? (E.preventDefault(), d(r[r.length - 1] ?? -1)) : E.key === "Enter" || E.key === " " ? (E.preventDefault(), o >= 0 && p(e[o])) : E.key === "Tab" && (E.preventDefault(), v());
  };
  return Bo.createPortal(g.jsxs(g.Fragment, { children: [g.jsx("div", { className: "rk-menu-scrim", onMouseDown: a, onContextMenu: (E) => {
    E.preventDefault(), a();
  } }), g.jsx("div", { ref: i, role: "menu", "aria-label": u, "aria-activedescendant": o >= 0 ? `${c.current}-${o}` : void 0, tabIndex: -1, className: "rk-menu", style: y, onMouseDown: (E) => E.preventDefault(), onKeyDown: T, children: e.map((E, r) => g.jsxs("div", { children: [E.separatorBefore ? g.jsx("div", { className: "rk-menu__sep", role: "separator" }) : null, g.jsxs("div", { id: `${c.current}-${r}`, role: E.checked === void 0 ? "menuitem" : "menuitemcheckbox", "aria-checked": E.checked, "aria-disabled": E.disabled || void 0, className: `rk-menu__item${r === o ? " is-active" : ""}${E.danger ? " is-danger" : ""}${E.disabled ? " is-disabled" : ""}`, onMouseMove: () => !E.disabled && d(r), onClick: () => p(E), children: [g.jsx("span", { className: "rk-menu__lead", "aria-hidden": "true", children: E.checked ? "✓" : E.icon }), g.jsx("span", { className: "rk-menu__label", children: E.label }), E.shortcut ? g.jsx(hu, { chord: E.shortcut, gap: 1 }) : null] })] }, E.id)) })] }), document.body);
}
const mv = { info: "i", success: "✓", warning: "!", error: "!" }, hv = 150;
function yv({ toast: l, leaving: t, onDismiss: e }) {
  return g.jsxs("div", { role: l.kind === "error" ? "alert" : "status", "data-kind": l.kind, className: `rk-toast rk-toast--${l.kind}${t ? " is-leaving" : ""}`, children: [g.jsx("span", { className: "rk-toast__glyph", "aria-hidden": "true", children: mv[l.kind] }), g.jsx("span", { className: "rk-toast__msg", children: l.message }), l.action ? g.jsx("button", { type: "button", className: "rk-toast__action", onClick: () => {
    var a;
    (a = l.action) == null || a.run(), e();
  }, children: l.action.label }) : null, g.jsx("button", { type: "button", className: "rk-toast__x", "aria-label": "Dismiss notification", onClick: e, children: "✕" })] });
}
function vv({ bus: l = Ly }) {
  const [t, e] = A.useState([]), [a, u] = A.useState(() => /* @__PURE__ */ new Set()), n = A.useRef(/* @__PURE__ */ new Map()), i = A.useCallback((c) => {
    const f = n.current.get(c);
    f != null && window.clearTimeout(f), u((o) => o.has(c) ? o : new Set(o).add(c)), n.current.set(c, window.setTimeout(() => {
      e((o) => o.filter((d) => d.id !== c)), u((o) => {
        const d = new Set(o);
        return d.delete(c), d;
      }), n.current.delete(c);
    }, hv));
  }, []);
  return A.useEffect(() => {
    const c = n.current, f = l.subscribe((o) => {
      if (o.type === "dismiss")
        return i(o.id);
      const { toast: d } = o;
      e((y) => [...y.slice(-3), d]), d.ttl > 0 && c.set(d.id, window.setTimeout(() => i(d.id), d.ttl));
    });
    return () => {
      f();
      for (const o of c.values())
        window.clearTimeout(o);
      c.clear();
    };
  }, [l, i]), g.jsx("div", { className: "rk-toasts", "aria-live": "polite", "aria-atomic": "false", "data-rk-toasts": !0, children: t.map((c) => g.jsx(yv, { toast: c, leaving: a.has(c.id), onDismiss: () => i(c.id) }, c.id)) });
}
const gv = {
  placeholder: "Search or run a command…",
  title: "Command palette",
  empty: "No matches.",
  results: "Results"
};
function Sv({ commands: l, onClose: t, registry: e, labels: a, limit: u = 50 }) {
  const n = { ...gv, ...a }, [i, c] = A.useState(""), [f, o] = A.useState(ho), { closing: d, close: y } = Hf(t, { escape: !1 }), h = A.useRef(null), v = A.useRef(null), p = A.useId();
  Cf(h, !0, { autoFocus: !1 }), A.useEffect(() => {
    var _;
    return (_ = v.current) == null ? void 0 : _.focus();
  }, []);
  const T = A.useMemo(() => {
    const _ = qy(l, i, u);
    return mo(_).flatMap((O) => O.items);
  }, [l, i, u]), E = A.useMemo(() => mo(T), [T]), r = Math.min(f.active, Math.max(0, T.length - 1));
  A.useEffect(() => {
    var _, O;
    (O = (_ = document.getElementById(`${p}-opt-${r}`)) == null ? void 0 : _.scrollIntoView) == null || O.call(_, { block: "nearest" });
  }, [r, p]);
  const s = (_) => {
    _ && (y(), window.setTimeout(() => _.run(), 90));
  }, m = (_) => (_.shortcut ? e == null ? void 0 : e.chordFor(_.shortcut) : void 0) ?? _.chord;
  let S = 0;
  return g.jsx("div", { className: `rk-overlay rk-overlay--top${d ? " is-closing" : ""}`, onMouseDown: (_) => _.target === _.currentTarget && y(), children: g.jsxs("div", { ref: h, role: "dialog", "aria-modal": "true", "aria-label": n.title, className: `rk-palette${d ? " is-closing" : ""}`, onKeyDown: (_) => {
    _.key === "Escape" && (_.preventDefault(), _.stopPropagation(), y());
  }, children: [g.jsxs("div", { className: "rk-palette__field", children: [g.jsxs("svg", { viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", strokeWidth: "1.4", strokeLinecap: "round", "aria-hidden": "true", children: [g.jsx("circle", { cx: "7", cy: "7", r: "4.5" }), g.jsx("path", { d: "M10.5 10.5 14 14" })] }), g.jsx("input", { ref: v, role: "combobox", "aria-label": n.placeholder, "aria-expanded": "true", "aria-controls": `${p}-list`, "aria-activedescendant": T.length ? `${p}-opt-${r}` : void 0, placeholder: n.placeholder, value: i, onChange: (_) => {
    c(_.target.value), o(ho);
  }, onKeyDown: (_) => {
    _.key === "ArrowDown" || _.key === "ArrowUp" ? (_.preventDefault(), o(Qy({ ...f, active: r }, _.key === "ArrowDown" ? "down" : "up", T.length))) : _.key === "Enter" && (_.preventDefault(), s(T[r]));
  } })] }), g.jsxs("div", { id: `${p}-list`, role: "listbox", "aria-label": n.results, className: "rk-palette__list", children: [T.length === 0 ? g.jsx("div", { className: "rk-palette__empty", children: n.empty }) : null, E.map((_) => g.jsxs("div", { role: "group", "aria-label": _.name, children: [g.jsx("div", { className: "rk-palette__group", "aria-hidden": "true", children: _.name }), _.items.map((O) => {
    const z = S++, x = m(O);
    return g.jsxs("div", { id: `${p}-opt-${z}`, role: "option", "aria-selected": z === r, "data-command": O.id, className: `rk-palette__opt${z === r ? " is-active" : ""}`, onMouseMove: (j) => o((U) => Gy({ ...U, active: U.active }, z, { x: j.clientX, y: j.clientY })), onClick: () => s(O), children: [O.icon ? g.jsx("span", { className: "rk-palette__icon", "aria-hidden": "true", children: O.icon }) : null, g.jsx("span", { className: "rk-palette__label", children: O.label }), x ? g.jsx(hu, { chord: xm(x)[0], gap: 1, platform: e == null ? void 0 : e.platform }) : O.hint ? g.jsx("span", { className: "rk-palette__hint", children: O.hint }) : null] }, O.id);
  })] }, _.name))] })] }) });
}
function bv({ registry: l, onClose: t, title: e = "Keyboard shortcuts" }) {
  const a = l.groups(), u = A.useId(), n = A.useRef(null), { closing: i, close: c } = Hf(t, { escape: !0 });
  return Cf(n, !0), g.jsx("div", { className: `rk-overlay rk-overlay--center${i ? " is-closing" : ""}`, onMouseDown: (f) => f.target === f.currentTarget && c(), children: g.jsxs("div", { ref: n, role: "dialog", "aria-modal": "true", "aria-labelledby": u, className: `rk-modal rk-help${i ? " is-closing" : ""}`, children: [g.jsxs("div", { className: "rk-help__head", children: [g.jsx("h2", { id: u, className: "rk-modal__title", children: e }), g.jsx("button", { type: "button", className: "rk-icon-btn", "aria-label": "Close", title: "Close", onClick: c, children: "✕" })] }), g.jsx("div", { className: "rk-help__grid", children: a.map((f) => g.jsxs("section", { children: [g.jsx("h3", { children: f.title }), g.jsx("ul", { children: f.items.map((o) => g.jsxs("li", { "data-shortcut": o.id, children: [g.jsx("span", { children: o.label }), g.jsx("span", { className: "rk-help__keys", children: xm(o.chords[0]).map((d, y) => g.jsxs("span", { className: "rk-help__step", children: [y > 0 ? g.jsx("em", { children: "then" }) : null, g.jsx(hu, { chord: d, gap: 1, platform: l.platform })] }, y)) })] }, o.id)) })] }, f.title)) })] }) });
}
const Bf = A.createContext(null);
function _v({ children: l, themeStore: t, registry: e, toastBus: a, bridge: u, platform: n, paletteChord: i = "mod+k", helpChord: c = "?", paletteLabels: f, helpTitle: o, mountOverlays: d = !0 }) {
  const y = n ?? da(), h = A.useMemo(() => e ?? nv({ platform: y }), [e, y]), v = A.useMemo(() => iv(), []), p = A.useMemo(() => a ?? Om(), [a]), T = A.useMemo(() => t ?? Ry({ storageKey: "rightkit.theme" }), [t]), [E, r] = A.useState(!1), [s, m] = A.useState(!1), [S, _] = A.useState([]), O = mu(u), z = A.useMemo(() => ({ open: E, show: () => r(!0), hide: () => r(!1), toggle: () => r((D) => !D) }), [E]), x = A.useMemo(() => ({ open: s, show: () => m(!0), hide: () => m(!1), toggle: () => m((D) => !D) }), [s]), j = A.useCallback((D) => new Promise((N) => _((X) => [...X, { kind: "confirm", options: D, resolve: N }])), []), U = A.useCallback((D) => new Promise((N) => _((X) => [...X, { kind: "prompt", options: D, resolve: N }])), []);
  A.useEffect(() => T.apply(T.read()), [T]), A.useEffect(() => h.attach(window), [h]), A.useEffect(() => {
    const D = [];
    return i && D.push(h.register({ id: "shell.palette", chord: i, label: "Command palette", group: "General", allowInInput: !0, run: () => r((N) => !N) })), c && D.push(h.register({
      id: "shell.help",
      chord: c,
      label: "Keyboard shortcuts",
      group: "General",
      run: () => m((N) => !N)
    })), () => D.forEach((N) => N());
  }, [h, i, c]);
  const B = u == null ? void 0 : u.onMenuAction;
  A.useEffect(() => {
    if (!B)
      return;
    let D, N = !1;
    return Promise.resolve(B((X) => {
      v.run(X) || h.invoke(X);
    })).then((X) => {
      N ? X == null || X() : D = X;
    }), () => {
      N = !0, D == null || D();
    };
  }, [B, v, h]);
  const rl = A.useSyncExternalStore(T.subscribe, T.read, T.read);
  A.useEffect(() => {
    var D, N;
    (N = (D = O.current) == null ? void 0 : D.setNativeTheme) == null || N.call(D, rl);
  }, [rl, O]);
  const _e = A.useMemo(() => ({ platform: y, registry: h, commands: v, toasts: p, bridge: u, themeStore: T, palette: z, help: x, confirm: j, prompt: U }), [y, h, v, p, u, T, z, x, j, U]), ma = A.useSyncExternalStore(v.subscribe, v.list, v.list), El = S[S.length - 1], M = (D) => {
    D(), _((N) => N.slice(0, -1));
  };
  return g.jsxs(Bf.Provider, { value: _e, children: [l, d ? g.jsxs(g.Fragment, { children: [E ? g.jsx(Sv, { commands: ma, registry: h, labels: f, onClose: z.hide }) : null, s ? g.jsx(bv, { registry: h, title: o, onClose: x.hide }) : null, (El == null ? void 0 : El.kind) === "confirm" ? g.jsx(ov, { ...El.options, onConfirm: () => M(() => El.resolve(!0)), onCancel: () => M(() => El.resolve(!1)) }) : null, (El == null ? void 0 : El.kind) === "prompt" ? g.jsx(rv, { ...El.options, onSubmit: (D) => M(() => El.resolve(D)), onCancel: () => M(() => El.resolve(null)) }) : null, g.jsx(vv, { bus: p })] }) : null] });
}
function Um() {
  return A.useContext(Bf);
}
function Yf() {
  const l = A.useContext(Bf);
  if (!l)
    throw new Error("useShell must be used inside <ShellProvider> (or <AppShell>).");
  return l;
}
function Ev(l) {
  const { commands: t } = Yf(), e = mu(l), a = l.map((u) => [u.id, u.label, u.hint ?? "", u.group ?? "", u.disabled ? 1 : 0, u.shortcut ?? "", u.chord ?? ""].join("\0")).join("");
  A.useEffect(() => {
    const u = (e.current ?? []).map((n) => ({ ...n, run: () => {
      var i;
      return (i = (e.current ?? []).find((c) => c.id === n.id)) == null ? void 0 : i.run();
    } }));
    return t.register(u);
  }, [t, a, e]);
}
function pv(l) {
  const { registry: t } = Yf(), e = mu(l), a = l.map((u) => [u.id, typeof u.chord == "string" ? u.chord : u.chord.join("|"), u.label, u.group ?? ""].join("\0")).join("");
  A.useEffect(() => {
    const u = (i) => (e.current ?? []).find((c) => c.id === i), n = (e.current ?? []).map((i) => t.register({ ...i, run: (c) => {
      var f;
      return (f = u(i.id)) == null ? void 0 : f.run(c);
    }, when: i.when ? () => {
      var c, f;
      return ((f = (c = u(i.id)) == null ? void 0 : c.when) == null ? void 0 : f.call(c)) ?? !0;
    } : void 0 }));
    return () => n.forEach((i) => i());
  }, [t, a, e]);
}
function Tv() {
  const { themeStore: l } = Yf(), t = A.useSyncExternalStore(l.subscribe, l.read, l.read), e = Nm("(prefers-color-scheme: dark)"), a = A.useCallback((u) => l.save(u), [l]);
  return { choice: t, resolved: Hy(t, e), setChoice: a };
}
const aa = {
  jumpTo: "Jump to",
  sections: "Sections",
  settings: "Settings",
  appearance: "Appearance",
  toggleSidebar: "Toggle sidebar",
  goTo: "Go to",
  theme: { system: "System", light: "Light", dark: "Dark" }
}, Kn = (l) => g.jsx("svg", { viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", strokeWidth: "1.4", strokeLinecap: "round", strokeLinejoin: "round", "aria-hidden": "true", children: l }), Cm = () => Kn(g.jsxs(g.Fragment, { children: [g.jsx("circle", { cx: "7", cy: "7", r: "4.5" }), g.jsx("path", { d: "M10.5 10.5 14 14" })] })), zv = () => Kn(g.jsxs(g.Fragment, { children: [g.jsx("circle", { cx: "8", cy: "8", r: "2.2" }), g.jsx("path", { d: "M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4" })] })), Av = () => Kn(g.jsxs(g.Fragment, { children: [g.jsx("circle", { cx: "8", cy: "8", r: "6" }), g.jsx("path", { d: "M8 2a6 6 0 0 1 0 12z", fill: "currentColor" })] })), Mv = () => Kn(g.jsxs(g.Fragment, { children: [g.jsx("rect", { x: "2", y: "3", width: "12", height: "10", rx: "2" }), g.jsx("path", { d: "M6 3v10" })] })), Ov = "M12 0V12H0A12 12 0 0 0 12 0Z";
function Dv() {
  return g.jsx(g.Fragment, { children: ["top", "bottom"].map((l) => g.jsx("svg", { className: `rk-fuse rk-fuse--${l}`, "data-rk-fuse": l, "aria-hidden": "true", focusable: "false", width: "12", height: "12", viewBox: "0 0 12 12", children: g.jsx("path", { d: Ov }) }, l)) });
}
const ji = (l) => (t) => {
  t.currentTarget.blur(), l().catch(() => {
  });
};
function xv({ adapter: l, labels: t = Fy() }) {
  const e = A.useMemo(() => Iy(l), [l]);
  return g.jsxs("div", { className: "rk-caption", children: [g.jsx("button", { type: "button", className: "rk-caption__btn", title: t.minimize, "aria-label": t.minimize, onClick: ji(e.minimize), children: g.jsx("svg", { width: "10", height: "10", viewBox: "0 0 10 10", "aria-hidden": "true", children: g.jsx("rect", { x: "0", y: "4.5", width: "10", height: "1", fill: "currentColor" }) }) }), g.jsx("button", { type: "button", className: "rk-caption__btn", title: t.maximize, "aria-label": t.maximize, onClick: ji(e.toggleMaximize), children: g.jsx("svg", { width: "10", height: "10", viewBox: "0 0 10 10", "aria-hidden": "true", children: g.jsx("rect", { x: "0.5", y: "0.5", width: "9", height: "9", fill: "none", stroke: "currentColor" }) }) }), g.jsx("button", { type: "button", className: "rk-caption__btn rk-caption__close", title: t.close, "aria-label": t.close, onClick: ji(e.close), children: g.jsx("svg", { width: "10", height: "10", viewBox: "0 0 10 10", "aria-hidden": "true", children: g.jsx("path", { d: "M0.5 0.5 9.5 9.5M9.5 0.5 0.5 9.5", stroke: "currentColor", strokeWidth: "1" }) }) })] });
}
function Nv({ start: l, startWidth: t, title: e, end: a, windowControls: u, windowControlLabels: n, platform: i, className: c }) {
  const f = Vn(), d = $y(i ?? f), y = t !== void 0 ? { width: t } : void 0;
  return g.jsxs("header", { className: `rk-top${c ? ` ${c}` : ""}`, "data-tauri-drag-region": !0, children: [g.jsxs("div", { className: "rk-top__side", style: y, "data-tauri-drag-region": !0, children: [d.nativeTrafficLights ? g.jsx("span", { className: "rk-top__lights", "data-tauri-drag-region": !0 }) : null, l] }), e ? g.jsx("div", { className: "rk-top__title", "data-tauri-drag-region": !0, children: e }) : null, g.jsx("div", { className: "rk-top__drag", "data-tauri-drag-region": !0 }), a, d.customCaptionButtons && u ? g.jsx(xv, { adapter: u, labels: n }) : null] });
}
function jv({ platform: l, className: t, style: e, children: a }) {
  const u = Vn(), n = l ?? u;
  return g.jsx("div", { className: `rk-shell${t ? ` ${t}` : ""}`, "data-platform": n, style: e, children: a });
}
function Uv({ children: l, className: t }) {
  return g.jsx("section", { className: `rk-plane${t ? ` ${t}` : ""}`, "data-rk-plane": !0, children: l });
}
function Cv({ value: l, onChange: t, labels: e = aa }) {
  const [a, u] = A.useState(null), n = Mm.map((i) => ({ id: i, label: e.theme[i], checked: l === i, run: () => t(i) }));
  return g.jsxs(g.Fragment, { children: [g.jsx(xc, { label: e.appearance, pressed: a !== null, "aria-haspopup": "menu", onClick: (i) => {
    const c = i.currentTarget.getBoundingClientRect();
    u({ x: c.right - 160, y: c.top - n.length * 28 - 16 });
  }, children: g.jsx(Av, {}) }), a ? g.jsx(dv, { x: a.x, y: a.y, width: 160, items: n, label: e.appearance, onClose: () => u(null) }) : null] });
}
function Hv({ groups: l, query: t, onQueryChange: e, onNavigate: a, label: u = aa.jumpTo, platform: n }) {
  const i = A.useRef(null);
  return g.jsxs("label", { className: "rk-search", children: [g.jsx(Cm, {}), g.jsx("input", { ref: i, "aria-label": u, placeholder: `${u}…`, value: t, onChange: (c) => e(c.target.value), onKeyDown: (c) => {
    if (c.key === "Enter") {
      const f = jy(zm(l, t));
      f && (a(f.id), e(""));
    }
    c.key === "Escape" && e("");
  } }), g.jsx(hu, { chord: "mod+f", platform: n, gap: 3 })] });
}
function Rv({ onOpen: l, label: t = aa.jumpTo, chord: e = "mod+k", platform: a }) {
  return g.jsxs("button", { type: "button", className: "rk-search rk-search--trigger", "aria-label": `${t} (command palette)`, "aria-haspopup": "dialog", onClick: l, children: [g.jsx(Cm, {}), g.jsxs("span", { className: "rk-search__text", children: [t, "…"] }), g.jsx(hu, { chord: e, platform: a, gap: 3 })] });
}
function Bv({ groups: l, activeId: t, onNavigate: e, label: a = aa.sections, renderItem: u }) {
  const n = A.useRef(null), i = Am(l), c = (f) => {
    var h, v;
    const o = Array.from(((h = n.current) == null ? void 0 : h.querySelectorAll("button[data-nav-id]")) ?? []), d = o.indexOf(document.activeElement), y = Uy("vertical", f.key, d < 0 ? 0 : d, o.length);
    y !== null && (f.preventDefault(), (v = o[y]) == null || v.focus());
  };
  return g.jsx("nav", { className: "rk-nav", "aria-label": a, ref: n, onKeyDown: c, children: l.map((f, o) => g.jsxs("div", { className: "rk-group", children: [f.title ? g.jsx("h6", { children: f.title }) : null, f.items.filter((d) => !d.hidden).map((d) => {
    const y = t === d.id, h = g.jsxs("button", { type: "button", "data-nav-id": d.id, className: `rk-item${y ? " is-active" : ""}`, "aria-current": y ? "page" : void 0, tabIndex: y || !i.some((v) => v.id === t) && d === i[0] ? 0 : -1, onClick: () => e(d.id), children: [d.icon, g.jsx("span", { className: "rk-item__label", children: d.label }), d.badge ? g.jsx("span", { className: "rk-item__badge", children: d.badge }) : null, y ? g.jsx(Dv, {}) : null] }, d.id);
    return u ? g.jsx("div", { children: u(d, { active: y }, h) }, d.id) : h;
  })] }, f.title ?? `g${o}`)) });
}
function Yv(l) {
  var j, U;
  const t = Vn(), e = l.platform ?? t, a = { ...aa, ...l.labels, theme: { ...aa.theme, ...(j = l.labels) == null ? void 0 : j.theme } }, u = Um(), n = Tv(), i = l.theme ?? n.choice, c = l.onThemeChange ?? n.setChoice, f = Nm("(max-width: 719px)"), [o, d] = A.useState(!1), y = l.sidebarCollapsed ?? o, h = (B) => {
    var rl;
    d(B), (rl = l.onSidebarCollapsedChange) == null || rl.call(l, B);
  }, v = l.sidebar !== !1, [p, T] = A.useState(""), E = A.useMemo(() => zm(l.groups, p), [l.groups, p]), r = ((U = l.bridge) == null ? void 0 : U.window) ?? l.windowControls, s = l.searchMode ?? "palette", m = l.sidebarToggle ?? !0, S = A.useMemo(
    () => [
      ...Am(l.groups).map((B) => ({ id: `nav:${B.id}`, label: B.label, group: a.goTo, icon: B.icon, keywords: B.keywords, run: () => l.onNavigate(B.id) })),
      ...l.onOpenSettings ? [{ id: "nav:settings", label: a.settings, group: a.goTo, keywords: ["preferences"], run: () => {
        var B;
        return (B = l.onOpenSettings) == null ? void 0 : B.call(l);
      } }] : [],
      ...Mm.map((B) => ({ id: `theme:${B}`, label: `${a.appearance}: ${a.theme[B]}`, group: a.appearance, keywords: ["theme", "dark", "light"], run: () => c(B) })),
      ...v && m ? [{ id: "shell.sidebar.toggle", label: a.toggleSidebar, group: "View", shortcut: "shell.sidebar", run: () => h(!y) }] : [],
      ...l.commands ?? []
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [l.groups, l.commands, l.onOpenSettings, a.goTo, a.settings, a.appearance, a.toggleSidebar, v, m, y]
  );
  Ev(S), pv(v && m ? [{ id: "shell.sidebar", chord: "mod+\\", label: a.toggleSidebar, group: "View", rebindable: !0, allowInInput: !0, run: () => h(!y) }] : []);
  const _ = l.sidebarWidth ? { "--rk-sidebar-width": `${l.sidebarWidth}px` } : void 0, O = y, z = v && m ? g.jsx(xc, { label: a.toggleSidebar, pressed: !O, onClick: () => h(!y), className: "rk-top__toggle", children: g.jsx(Mv, {}) }) : null, x = g.jsxs("aside", { className: "rk-side", "aria-label": a.sections, hidden: O, children: [s === "palette" ? g.jsx(Rv, { onOpen: u.palette.show, label: a.jumpTo, platform: e }) : null, s === "filter" ? g.jsx(Hv, { groups: l.groups, query: p, onQueryChange: T, onNavigate: l.onNavigate, label: a.jumpTo, platform: e }) : null, l.sidebarTop, g.jsx(Bv, { groups: E, activeId: l.activeId, onNavigate: (B) => {
    l.onNavigate(B), f && h(!0);
  }, label: a.sections }), g.jsxs("div", { className: "rk-side__foot", children: [l.statusFooter ? g.jsx("div", { className: "rk-status", children: l.statusFooter }) : null, g.jsxs("footer", { className: "rk-brand-foot", children: [g.jsx("span", { className: "rk-wordmark", children: l.wordmark }), l.onOpenSettings ? g.jsx(xc, { label: a.settings, pressed: l.settingsActive, onClick: l.onOpenSettings, children: g.jsx(zv, {}) }) : null, g.jsx(Cv, { value: i, onChange: c, labels: a })] })] })] });
  return g.jsxs(jv, { platform: e, className: `rk-shell--fused${O ? " is-collapsed" : ""}${f ? " is-narrow" : ""}${v ? "" : " no-sidebar"}${l.className ? ` ${l.className}` : ""}`, style: _, children: [l.titlebar === !1 ? null : l.titlebar !== void 0 ? l.titlebar : g.jsx(Nv, { start: g.jsxs(g.Fragment, { children: [z, l.titlebarStart] }), startWidth: O || !v ? "auto" : void 0, title: l.title, end: l.titlebarEnd, windowControls: r, windowControlLabels: l.windowControlLabels, platform: e }), g.jsxs("div", { className: "rk-body", children: [v ? l.sidebar ?? x : null, g.jsx(Uv, { children: l.children })] })] });
}
function qv(l) {
  const t = Um(), e = Vn(), a = g.jsx(Yv, { ...l });
  return t ? a : g.jsx(_v, { ...l.shell, bridge: l.bridge, platform: l.platform ?? e, children: a });
}
const Te = {
  menu: "rightkit-shell://menu",
  tray: "rightkit-shell://tray",
  deepLink: "rightkit-shell://deep-link",
  secondInstance: "rightkit-shell://second-instance",
  stall: "rightkit-shell://stall",
  engine: "rightkit-shell://engine"
};
function Gv() {
  const l = globalThis.__TAURI_INTERNALS__;
  if (!l)
    throw new Error("rightkit-shell: not running inside a Tauri webview");
  return {
    invoke: (t, e) => l.invoke(t, e),
    listen: async (t, e) => {
      const a = l.transformCallback((n) => e(n.payload)), u = await l.invoke("plugin:event|listen", { event: t, target: { kind: "Any" }, handler: a });
      return () => {
        l.invoke("plugin:event|unlisten", { event: t, eventId: u });
      };
    }
  };
}
const So = (l) => {
  let t = "";
  for (let e = 0; e < l.length; e += 32768)
    t += String.fromCharCode(...l.subarray(e, e + 32768));
  return btoa(t);
}, bo = (l) => Uint8Array.from(atob(l), (t) => t.charCodeAt(0));
function Qv(l = Gv()) {
  const { invoke: t, listen: e } = l, a = {
    /** Tell the host the UI is listening: queued deep links are delivered now. Call once after subscribing. */
    ready: () => t("rk_shell_ready"),
    status: () => t("rk_shell_status"),
    quit: () => t("rk_shell_quit"),
    /** Open an http(s)/mailto URL in the system browser. Other schemes are refused by the host. */
    openExternal: (n) => t("rk_shell_open_external", { url: n }),
    window: {
      do: (n) => t("rk_shell_window", { action: n }),
      minimize: () => t("rk_shell_window", { action: "minimize" }),
      toggleMaximize: () => t("rk_shell_window", { action: "toggle-maximize" }),
      close: () => t("rk_shell_window", { action: "close" }),
      startDrag: () => t("rk_shell_window", { action: "start-drag" }),
      open: (n) => t("rk_shell_open_window", { spec: n }),
      /** macOS: nudge the traffic lights from their default origin (dx right, dy down). */
      trafficLights: (n, i) => t("rk_shell_traffic_lights", { dx: n, dy: i })
    },
    menu: {
      setEnabled: (n, i) => t("rk_shell_set_menu_item_enabled", { id: n, enabled: i }),
      onClick: (n) => e(Te.menu, (i) => n(i.id))
    },
    tray: {
      update: (n) => t("rk_shell_set_tray", { update: n }),
      onEvent: (n) => e(Te.tray, n)
    },
    deepLinks: {
      on: (n) => e(Te.deepLink, n)
    },
    onSecondInstance: (n) => e(Te.secondInstance, n),
    onStall: (n) => e(Te.stall, n),
    autostart: {
      get: () => t("rk_shell_autostart", { enabled: null }),
      set: (n) => t("rk_shell_autostart", { enabled: n })
    },
    clipboard: {
      read: async (n) => {
        const i = await t("rk_shell_clipboard_read", { board: n ?? null });
        return {
          text: i.text ?? void 0,
          html: i.html ?? void 0,
          png: i.png ? bo(i.png) : void 0,
          files: i.files ?? [],
          custom: Object.fromEntries(Object.entries(i.custom ?? {}).map(([c, f]) => [c, bo(f)])),
          board: n
        };
      },
      write: (n) => t("rk_shell_clipboard_write", {
        payload: {
          text: n.text ?? null,
          html: n.html ?? null,
          png: n.png ? So(n.png) : null,
          files: n.files ?? [],
          custom: Object.fromEntries(Object.entries(n.custom ?? {}).map(([i, c]) => [i, So(c)])),
          board: n.board ?? null
        }
      })
    },
    engine: {
      /** One engine call over the stdio contract. Events stream through `onEvent` meanwhile. */
      call: (n, i) => t("rk_shell_engine_call", { method: n, params: i ?? null }),
      /** `call === '$lifecycle'` carries `{state:'ready'|'error', ...}` for engine start; `ready()` replays the last one. */
      onEvent: (n) => e(Te.engine, n)
    }
  };
  return { ...a, bridge: () => ({
    window: {
      minimize: () => a.window.minimize(),
      toggleMaximize: () => a.window.toggleMaximize(),
      close: () => a.window.close()
    },
    openExternal: (n) => a.openExternal(n),
    // One id stream for native menu items, tray menu items and deep links (`deep-link:<raw url>`),
    // which is what app-shell routes to a command or shortcut with the same id.
    onMenuAction: async (n) => {
      const i = await Promise.all([
        a.menu.onClick(n),
        a.tray.onEvent((c) => {
          c.kind === "menu" && n(c.id);
        }),
        a.deepLinks.on((c) => n(`deep-link:${c.raw}`))
      ]);
      return () => i.forEach((c) => c());
    },
    setNativeTheme: (n) => t("rk_shell_set_theme", { theme: n })
  }) };
}
const Xv = Qv(), _o = Xv.bridge();
window.__appshell = { ran: [], themes: [], errors: [] };
const Hm = {
  ..._o,
  setNativeTheme: async (l) => {
    window.__appshell.themes.push(l), await _o.setNativeTheme(l).catch((t) => void window.__appshell.errors.push(String(t)));
  }
};
window.__bridge = Hm;
Ny.createRoot(document.getElementById("appshell-root")).render(
  /* @__PURE__ */ g.jsx(
    qv,
    {
      bridge: Hm,
      groups: [{ items: [{ id: "home", label: "Home" }] }],
      activeId: "home",
      onNavigate: () => {
      },
      title: "app-shell on rightkit-shell",
      commands: [
        { id: "demo.ping", label: "Ping", run: () => void window.__appshell.ran.push("demo.ping") },
        { id: "tray.status", label: "Status", run: () => void window.__appshell.ran.push("tray.status") },
        // Deep links reach app-shell as `deep-link:<raw url>` ids.
        { id: "deep-link:rkshell-example://open/doc/7", label: "Open doc 7", run: () => void window.__appshell.ran.push("deep-link:doc/7") }
      ],
      children: /* @__PURE__ */ g.jsx("div", { id: "appshell-ready", children: "app-shell mounted" })
    }
  )
);
