// A link like /fund names a screen of the app: send it to /#fund. Anything else keeps this page.
const screen = location.pathname.replace(/^\/+|\/+$/g, '');
if (['chat', 'fund', 'notes', 'close', 'agents'].includes(screen)) {
  location.replace('/#' + screen);
}
