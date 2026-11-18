package com.moyu.reader

import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // targetSdk 35+ 强制 edge-to-edge，状态栏会压在网页顶部。
    // 阅读器自己画米色纸面，这里把系统栏高度让出来，
    // 工具栏就不会被时间和手势条压住。
    val decor = window.decorView
    ViewCompat.setOnApplyWindowInsetsListener(decor) { v, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
      v.setPadding(0, bars.top, 0, bars.bottom)
      insets
    }
  }

  // 返回键：先问网页（阅读页→书架、目录打开→收起），网页说没得退再退出。
  @Suppress("DEPRECATION")
  override fun onBackPressed() {
    val web = findWebView(window.decorView)
    if (web == null) {
      super.onBackPressed()
      return
    }
    web.evaluateJavascript("window.__backAction ? window.__backAction() : false") { result ->
      if (result?.trim()?.trim('"') != "true") {
        runOnUiThread { finish() }
      }
    }
  }

  private fun findWebView(v: View): WebView? {
    if (v is WebView) return v
    if (v is ViewGroup) {
      for (i in 0 until v.childCount) {
        findWebView(v.getChildAt(i))?.let { return it }
      }
    }
    return null
  }
}
