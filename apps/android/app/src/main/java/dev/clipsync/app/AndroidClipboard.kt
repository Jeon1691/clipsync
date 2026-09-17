package dev.clipsync.app

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import uniffi.clipsync_mobile.ClipboardBridge
import java.io.ByteArrayOutputStream

class AndroidClipboard(private val app: Context) : ClipboardBridge {
    private val cm = app.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager

    override fun readText(): String? {
        val clip = cm.primaryClip ?: return null
        if (clip.itemCount == 0) return null
        return clip.getItemAt(0).coerceToText(app)?.toString()
    }

    override fun writeText(text: String) {
        cm.setPrimaryClip(ClipData.newPlainText("ClipSync", text))
    }

    override fun readImagePng(): ByteArray? {
        val clip = cm.primaryClip ?: return null
        if (clip.itemCount == 0) return null
        val uri = clip.getItemAt(0).uri ?: return null
        return try {
            app.contentResolver.openInputStream(uri)?.use { it.readBytes() }
        } catch (_: Exception) {
            null
        }
    }

    override fun writeImagePng(png: ByteArray) {
        val bmp = BitmapFactory.decodeByteArray(png, 0, png.size) ?: return
        val out = ByteArrayOutputStream()
        bmp.compress(Bitmap.CompressFormat.PNG, 100, out)
        writeText("[ClipSync image ${out.size()} bytes — open the ClipSync notification]")
        notify("ClipSync", "Received an image (${out.size()} bytes)")
    }

    override fun notify(title: String, body: String) {
        val n = NotificationCompat.Builder(app, ClipSyncApp.CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_menu_share)
            .setContentTitle(title)
            .setContentText(body)
            .setAutoCancel(true)
            .build()
        if (Build.VERSION.SDK_INT < 33 ||
            app.checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) ==
            android.content.pm.PackageManager.PERMISSION_GRANTED
        ) {
            NotificationManagerCompat.from(app).notify(title.hashCode(), n)
        }
    }
}
