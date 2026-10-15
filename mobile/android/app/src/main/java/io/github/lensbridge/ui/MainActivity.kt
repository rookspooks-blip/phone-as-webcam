package io.github.lensbridge.ui

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import io.github.lensbridge.pairing.PairingStore
import io.github.lensbridge.service.StreamService
import java.net.Inet4Address
import java.net.NetworkInterface

class MainActivity : ComponentActivity() {
    private val permissions = buildList {
        add(Manifest.permission.CAMERA)
        add(Manifest.permission.RECORD_AUDIO)
        if (Build.VERSION.SDK_INT >= 33) add(Manifest.permission.POST_NOTIFICATIONS)
    }.toTypedArray()

    private val askPermissions = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { startServiceIfAllowed() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val missing = permissions.filter { ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED }
        if (missing.isNotEmpty()) askPermissions.launch(missing.toTypedArray()) else startServiceIfAllowed()
        setContent {
            MaterialTheme(colorScheme = darkColorScheme()) {
                Surface(Modifier.fillMaxSize()) { Screen() }
            }
        }
    }

    private fun startServiceIfAllowed() {
        // A camera-type foreground service may only start with the CAMERA permission and from the foreground.
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            StreamService.start(this)
        }
    }

    private fun setDim(on: Boolean) {
        window.attributes = window.attributes.apply {
            screenBrightness = if (on) 0.01f else WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE
        }
        if (on) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON) else window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    }

    private fun ipAddresses(): List<String> = runCatching {
        NetworkInterface.getNetworkInterfaces().toList()
            .filter { it.isUp && !it.isLoopback }
            .flatMap { ni -> ni.inetAddresses.toList().filterIsInstance<Inet4Address>().map { "${it.hostAddress}  (${ni.name})" } }
    }.getOrDefault(emptyList())

    @SuppressLint("BatteryLife")
    private fun requestBatteryExemption() {
        startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")))
    }

    private fun batteryOptimized(): Boolean = !(getSystemService(POWER_SERVICE) as PowerManager).isIgnoringBatteryOptimizations(packageName)

    @Composable
    private fun Screen() {
        val state by StreamService.state.collectAsState()
        val pairing = remember { PairingStore.get(this) }
        val pcs by pairing.pcs.collectAsState()
        val prefs = remember { getSharedPreferences("settings", MODE_PRIVATE) }
        var dimmed by remember { mutableStateOf(false) }
        var refresh by remember { mutableStateOf(0) }
        var httpEnabled by remember { mutableStateOf(prefs.getBoolean("httpEnabled", false)) }
        var orientation by remember { mutableStateOf(prefs.getString("orientationMode", "auto")!!) }
        var rotationOverride by remember { mutableStateOf(prefs.getInt("rotationOverride", -1)) }
        var flipFront by remember { mutableStateOf(prefs.getBoolean("flipFront", true)) }
        var name by remember { mutableStateOf(prefs.getString("deviceName", null) ?: "${Build.MANUFACTURER} ${Build.MODEL}") }

        // The PC can toggle dimming ("screen": "dim"/"on"); the local button works too.
        LaunchedEffect(state.dimRequested) { dimmed = state.dimRequested }
        LaunchedEffect(dimmed) { setDim(dimmed) }

        fun saved() = StreamService.instance?.settingsChanged()

        Box(Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text("Lensbridge", style = MaterialTheme.typography.headlineMedium)

                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp)) {
                        Text(state.connectedPc?.let { "Передача на «$it»" } ?: if (state.running) "Ожидание подключения ПК" else "Сервер не запущен", style = MaterialTheme.typography.titleMedium)
                        Text(state.status, style = MaterialTheme.typography.bodySmall)
                        if (state.streaming) Text("${state.fps} к/с · ${"%.1f".format(state.kbps / 1000.0)} Мбит/с" + if (state.viewers > 0) " · браузер: ${state.viewers}" else "")
                        Spacer(Modifier.padding(4.dp))
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(onClick = { dimmed = true }) { Text("Затемнить экран") }
                            if (state.running) OutlinedButton(onClick = { StreamService.stop(this@MainActivity) }) { Text("Остановить") }
                            else OutlinedButton(onClick = { startServiceIfAllowed() }) { Text("Запустить") }
                        }
                        Text(
                            "Экран можно выключить кнопкой питания — трансляция продолжится (служба переднего плана + WakeLock).",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }

                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp)) {
                        Text("Адрес для ПК (порт 4748)", style = MaterialTheme.typography.titleMedium)
                        val ips = remember(refresh) { ipAddresses() }
                        if (ips.isEmpty()) Text("Нет сети")
                        ips.forEach { Text(it, fontFamily = FontFamily.Monospace) }
                        Text("USB: включите «Отладку по USB», ПК подключится через adb сам.", style = MaterialTheme.typography.bodySmall)
                        TextButton(onClick = { refresh++ }) { Text("Обновить") }
                    }
                }

                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp)) {
                        Text("Сопряжённые ПК", style = MaterialTheme.typography.titleMedium)
                        if (pcs.isEmpty()) Text("Нет. На ПК: Lensbridge → «Показать QR для телефона».", style = MaterialTheme.typography.bodySmall)
                        pcs.forEach { pc ->
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Column(Modifier.weight(1f)) {
                                    Text(pc.name)
                                    Text("ключ ${pc.keyIdHex}", style = MaterialTheme.typography.bodySmall)
                                }
                                TextButton(onClick = { pairing.remove(pc.keyIdHex) }) { Text("Удалить") }
                            }
                        }
                        Button(
                            enabled = !state.streaming,
                            onClick = { startActivity(Intent(this@MainActivity, ScanActivity::class.java)) },
                        ) { Text("Сопрячь с ПК (сканировать QR)") }
                        if (state.streaming) Text("Сначала отключите ПК: камера занята.", style = MaterialTheme.typography.bodySmall)
                    }
                }

                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text("Настройки", style = MaterialTheme.typography.titleMedium)
                        Text("Ориентация видео")
                        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            listOf("auto" to "Авто", "landscape" to "Альбомная", "portrait" to "Портретная").forEach { (k, label) ->
                                FilterChip(selected = orientation == k, onClick = {
                                    orientation = k
                                    prefs.edit().putString("orientationMode", k).apply()
                                    saved()
                                }, label = { Text(label) })
                            }
                        }
                        Text("Поворот вручную (если картинка на ПК повёрнута)")
                        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            listOf(-1, 0, 90, 180, 270).forEach { r ->
                                FilterChip(selected = rotationOverride == r, onClick = {
                                    rotationOverride = r
                                    prefs.edit().putInt("rotationOverride", r).apply()
                                    saved()
                                }, label = { Text(if (r < 0) "нет" else "$r°") })
                            }
                        }
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("Не зеркалить фронтальную камеру", Modifier.weight(1f))
                            Switch(checked = flipFront, onCheckedChange = {
                                flipFront = it
                                prefs.edit().putBoolean("flipFront", it).apply()
                                saved()
                            })
                        }
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Column(Modifier.weight(1f)) {
                                Text("Просмотр в браузере")
                                Text("http://IP:4747/video — без шифрования, только локальная сеть", style = MaterialTheme.typography.bodySmall)
                            }
                            Switch(checked = httpEnabled, onCheckedChange = {
                                httpEnabled = it
                                prefs.edit().putBoolean("httpEnabled", it).apply()
                                saved()
                            })
                        }
                        OutlinedTextField(value = name, onValueChange = {
                            name = it
                            prefs.edit().putString("deviceName", it).apply()
                        }, label = { Text("Имя телефона (видно на ПК после перезапуска)") }, modifier = Modifier.fillMaxWidth())
                    }
                }

                if (batteryOptimized()) {
                    Card(Modifier.fillMaxWidth()) {
                        Column(Modifier.padding(16.dp)) {
                            Text("Энергосбережение", style = MaterialTheme.typography.titleMedium)
                            Text(
                                "Некоторые прошивки (Xiaomi, Huawei, Samsung, OnePlus) убивают фоновые службы при выключенном экране. " +
                                    "Разрешите работу без ограничений; подробности для вашей модели: dontkillmyapp.com",
                                style = MaterialTheme.typography.bodySmall,
                            )
                            Button(onClick = { requestBatteryExemption() }) { Text("Отключить оптимизацию батареи") }
                        }
                    }
                }

                Text("События", style = MaterialTheme.typography.titleMedium)
                state.events.takeLast(20).reversed().forEach { Text(it, fontFamily = FontFamily.Monospace, fontSize = 12.sp) }
                Spacer(Modifier.width(1.dp).padding(24.dp))
            }

            if (dimmed) {
                Box(
                    Modifier.fillMaxSize().background(Color.Black).pointerInput(Unit) {
                        detectTapGestures(onDoubleTap = { dimmed = false })
                    },
                    contentAlignment = Alignment.Center,
                ) {
                    Text("Экран затемнён, камера работает\nДвойное касание — выйти", color = Color(0x44FFFFFF), textAlign = TextAlign.Center)
                }
            }
        }
    }
}
