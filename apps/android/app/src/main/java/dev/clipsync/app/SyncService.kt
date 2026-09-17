package dev.clipsync.app

import android.app.Service
import android.content.Intent
import android.os.IBinder
import androidx.core.app.NotificationCompat

class SyncService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val n = NotificationCompat.Builder(this, ClipSyncApp.CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_menu_share)
            .setContentTitle("ClipSync")
            .setContentText("Clipboard sync is running")
            .setOngoing(true)
            .build()
        startForeground(1, n)
        val app = application as ClipSyncApp
        try {
            app.client.startSync(AndroidClipboard(applicationContext))
        } catch (_: Exception) {
        }
        return START_STICKY
    }
}
