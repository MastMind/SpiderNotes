package app.spidernotes.fgservice;

import android.app.Activity;
import app.tauri.annotation.Command;
import app.tauri.annotation.TauriPlugin;
import app.tauri.plugin.Invoke;
import app.tauri.plugin.Plugin;

/**
 * Bridge for the Rust side: run_mobile_plugin("start"/"stop") lands here.
 */
@TauriPlugin
public class FgPlugin extends Plugin {
    private final Activity activity;

    public FgPlugin(Activity activity) {
        super(activity);
        this.activity = activity;
    }

    @Command
    public void start(Invoke invoke) {
        FgService.start(activity);
        invoke.resolve();
    }

    @Command
    public void stop(Invoke invoke) {
        FgService.stop(activity);
        invoke.resolve();
    }
}
