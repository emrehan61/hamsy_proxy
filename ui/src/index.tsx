// App entry point. @solidjs/router 0.15 has no `<Outlet/>` export — App is
// written to the router's `root` component contract instead (see
// App.tsx's header comment), so routes are wired as
// `<Router root={App}>` with `<Route>` children rendered into
// `props.children`.

import { render } from "solid-js/web";
import { Route, Router } from "@solidjs/router";
import App from "./App";
import Traffic from "./pages/Traffic";
import Rules from "./pages/Rules";
import Settings from "./pages/Settings";
import Setup from "./pages/Setup";

const root = document.getElementById("root");
if (!root) {
  throw new Error("flproxy: #root element not found in index.html");
}

render(
  () => (
    <Router root={App}>
      <Route path="/" component={Traffic} />
      <Route path="/rules" component={Rules} />
      <Route path="/rules/:id" component={Rules} />
      <Route path="/settings" component={Settings} />
      <Route path="/setup" component={Setup} />
    </Router>
  ),
  root,
);
