// QA mute (rightkit#1005): while RIGHTKIT_QA_MUTE=1 no media element of this page is audible.
// Injected by the control plugin, only in a `control` build with the QA control service active.
(function () {
  if (window.__RK_QA_MUTE__) return;
  window.__RK_QA_MUTE__ = true;
  var proto = window.HTMLMediaElement && window.HTMLMediaElement.prototype;
  if (proto && typeof proto.play === 'function') {
    var play = proto.play;
    proto.play = function () {
      this.muted = true;
      return play.apply(this, arguments);
    };
  }
  window.addEventListener('play', function (event) {
    var target = event.target;
    if (target && typeof target.muted === 'boolean') target.muted = true;
  }, true);
})();
