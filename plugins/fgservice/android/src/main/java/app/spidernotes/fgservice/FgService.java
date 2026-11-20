package app.spidernotes.fgservice;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.os.IBinder;

/**
 * Foreground service that keeps the process (and with it the mDNS/TCP sync
 * loops) alive while the app is in the background. Shows a persistent
 * notification; type "dataSync" per Android 14+ FGS rules.
 */
public class FgService extends Service {
    private static final int NOTIFICATION_ID = 1;
    private static final String CHANNEL_ID = "spidernotes-sync";

    public static void start(Context context) {
        // fail soft: a background-sync problem must never crash the app
        try {
            context.startForegroundService(new Intent(context, FgService.class));
        } catch (Exception e) {
            android.util.Log.e("FgService", "start failed", e);
        }
    }

    public static void stop(Context context) {
        try {
            context.stopService(new Intent(context, FgService.class));
        } catch (Exception e) {
            android.util.Log.e("FgService", "stop failed", e);
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        try {
            startForeground(NOTIFICATION_ID, buildNotification());
        } catch (Exception e) {
            android.util.Log.e("FgService", "startForeground failed", e);
            stopSelf();
        }
        return START_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private Notification buildNotification() {
        Notification.Builder builder;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            NotificationManager nm =
                (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
            NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID,
                "Background sync",
                NotificationManager.IMPORTANCE_MIN);
            channel.setShowBadge(false);
            nm.createNotificationChannel(channel);
            builder = new Notification.Builder(this, CHANNEL_ID);
        } else {
            builder = new Notification.Builder(this);
        }
        return builder
            .setContentTitle("SpiderNotes")
            .setContentText("Syncing notes in the background")
            .setOngoing(true)
            .setSmallIcon(android.R.drawable.ic_menu_save)
            .build();
    }
}
