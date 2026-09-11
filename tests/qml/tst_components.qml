import QtQuick
import QtTest
import qs.Commons

import "../../quickshell" as Whatsapp
import "../../quickshell/DevicePixel.js" as DevicePixel
import "fixtures"

TestCase {
  id: testCase
  name: "QmlComponents"

  Component {
    id: panelComponent
    Whatsapp.Panel {}
  }

  Component {
    id: barComponent
    Whatsapp.BarWidget {}
  }

  Component { id: serviceComponent; WorkflowService {} }
  Component { id: avatarComponent; Whatsapp.Avatar {} }
  Component { id: albumMosaicComponent; Whatsapp.AlbumMosaic {} }
  Component { id: crispSurfaceComponent; Whatsapp.DevicePixelBorderSurface {} }
  Component { id: crispTextFieldComponent; Whatsapp.DevicePixelTextField {} }
  Component { id: documentCardComponent; Whatsapp.DocumentCard {} }
  Component { id: locationCardComponent; Whatsapp.LocationCard {} }
  Component { id: stickerCardComponent; Whatsapp.StickerCard {} }
  Component { id: voiceMessageCardComponent; Whatsapp.VoiceMessageCard {} }
  Component { id: mediaPreviewCardComponent; Whatsapp.MediaPreviewCard {} }
  Component { id: pollCardComponent; Whatsapp.PollCard {} }
  Component { id: reactionPickerComponent; Whatsapp.ReactionPicker {} }
  Component { id: receiptTooltipComponent; Whatsapp.ReceiptTooltip {} }

  function lastCall(service, name) {
    for (var i = service.calls.length - 1; i >= 0; i--)
      if (service.calls[i].name === name) return service.calls[i]
    return null
  }

  function init() {
    failOnWarning(/.*/)
  }

  function test_panel_loads() {
    var service = createTemporaryObject(serviceComponent, testCase)
    service.chats = [{ jid: "group@g.us", name: "Test Group", is_group: true }]
    service.selectedChatJid = "group@g.us"
    service.groupParticipantsChatJid = "group@g.us"
    service.groupParticipants = [
      { jid: "me", name: "", is_me: true },
      { jid: "alice", name: "Alice" },
      { jid: "bob", name: "Bob" },
      { jid: "carol", name: "Carol" },
      { jid: "dave", name: "Dave" }
    ]
    var panel = createTemporaryObject(panelComponent, testCase, { service: service })
    verify(panel !== null)
    compare(panel.groupConversationSubtitle(), "You, Alice, Bob, Carol + 1 other")
    compare(panel.conversationActivitySubtitle(),
      "You, Alice, Bob, Carol + 1 other")
    service.chatStateLabelResult = "Alice is typing…"
    compare(panel.conversationChatActivity(), "Alice is typing…")
    compare(panel.conversationActivitySubtitle(), "Alice is typing…")
    compare(panel.sidebarChatActivity(service.selectedChat), "Alice is typing…")
    service.chatStateLabelResult = ""
    compare(panel.conversationChatActivity(), "")
    compare(panel.sidebarChatActivity(service.selectedChat), "")
    service.groupParticipantsError = "failed"
    compare(panel.groupConversationSubtitle(), "Participants unavailable")
    compare(panel.licenseViewer.kindLabel("project"), "Application")
    compare(panel.licenseViewer.kindLabel("asset"), "Bundled asset")
    compare(panel.licenseViewer.kindLabel("crate"), "Rust package")
    panel.licenseViewer.entries = [
      { name: "Alpha", version: "1.0", license: "MIT" },
      { name: "Beta", version: "2.0", license: "Apache-2.0" }
    ]
    compare(panel.licenseViewer.filtered("").length, 2)
    compare(panel.licenseViewer.filtered("apache")[0].name, "Beta")
    compare(panel.chatShortcutSlot(Qt.Key_1), 0)
    compare(panel.chatShortcutSlot(Qt.Key_0), 9)
    compare(panel.chatShortcutSlot(Qt.Key_A), -1)
    compare(panel.remainingTimeLabel(panel.currentTimestamp + 65), "2 minutes left")
    compare(panel.messageReceiptIcon(0), "󰥔")
    compare(panel.messageReceiptIcon(1), "✓")
    compare(panel.messageReceiptIcon(3), "✓✓")
    compare(panel.messageReceiptIcon(4), "󰍬")
    compare(panel.messageReceiptLabel(3), "Read")
    compare(panel.messageReceiptTimestamp(0), "")
    compare(panel.messageReceiptTooltip({ receipt: 3, read_by: [] }), "Read")
    var delivered = panel.messageReceiptTimestamp(100)
    var read = panel.messageReceiptTimestamp(120)
    compare(panel.messageReceiptTooltip({
      receipt: 2,
      delivered_to: [{
        jid: "dave@s.whatsapp.net", name: "Dave", delivered_at: 100
      }]
    }), "Delivered\nDave · " + delivered)
    compare(panel.messageReceiptTooltip({
      receipt: 3,
      delivered_at: 100,
      read_at: 120,
      delivered_to: [
        { jid: "alice@s.whatsapp.net", name: "Alice", delivered_at: 100 }
      ],
      read_by: [
        { jid: "alice@s.whatsapp.net", name: "Alice", read_at: 120 }
      ]
    }), "Read\nAlice · " + read)
    compare(panel.messageReceiptTooltip({ receipt: 3, read_by: [
      { jid: "alice@s.whatsapp.net", name: "Alice" }
    ] }), "Read\nAlice")
    compare(panel.messageReceiptTooltip({ receipt: 3, read_by: [
      { jid: "bob@s.whatsapp.net", name: "Bob" },
      { jid: "alice@s.whatsapp.net", name: "Alice" },
      { jid: "alice@s.whatsapp.net", name: "Duplicate" }
    ] }), "Read\nAlice\nBob")
    var mixedGroups = panel.messageReceiptGroups({
      receipt: 3,
      delivered_to: [
        { jid: "alice@s.whatsapp.net", name: "Alice", delivered_at: 100 },
        { jid: "dave@s.whatsapp.net", name: "Dave", delivered_at: 100 },
        { jid: "dave@s.whatsapp.net", name: "Duplicate", delivered_at: 101 }
      ],
      read_by: [
        { jid: "alice@s.whatsapp.net", name: "Alice", read_at: 120 }
      ]
    })
    compare(mixedGroups.length, 2)
    compare(mixedGroups[0].label, "Read")
    compare(mixedGroups[0].entries[0], "Alice · " + read)
    compare(mixedGroups[1].label, "Delivered")
    compare(mixedGroups[1].entries[0], "Dave · " + delivered)
    compare(panel.messageReceiptTooltip({
      receipt: 3,
      delivered_to: [
        { jid: "alice@s.whatsapp.net", name: "Alice", delivered_at: 100 },
        { jid: "dave@s.whatsapp.net", name: "Dave", delivered_at: 100 }
      ],
      read_by: [
        { jid: "alice@s.whatsapp.net", name: "Alice", read_at: 120 }
      ]
    }), "Read\nAlice · " + read + "\n\nDelivered\nDave · " + delivered)

    service.chats = [{
      jid: "alice@s.whatsapp.net", name: "Alice", phone_number: "316123",
      is_group: false
    }]
    service.selectedChatJid = "alice@s.whatsapp.net"
    service.presenceLabelResult = "online"
    compare(panel.conversationActivitySubtitle(), "online")
    service.chatStateLabelResult = "typing…"
    compare(panel.sidebarChatActivity(service.selectedChat), "Typing…")
    service.chatStateLabelResult = ""
    service.presenceLabelResult = ""
    compare(panel.conversationActivitySubtitle(), "+316123")
  }

  function test_device_pixel_border_snapping() {
    var none = DevicePixel.borderSpec(Border.none(), 2)
    compare(none.widths.top, 0)
    compare(none.widths.left, 0)
    var hairline = DevicePixel.borderSpec(Border.flat("#ff0000", 0.4), 2)
    compare(hairline.widths.top, 0.5)
    compare(hairline.widths.right, 0.5)
    compare(String(hairline.color), "#ff0000")
    compare(hairline.gradient.enabled, true)
    compare(String(hairline.gradient.colors[0]), "#ff0000")
    compare(DevicePixel.borderSpec(Border.flat("#ff0000", 3), 1).widths.top, 3)
    compare(DevicePixel.borderSpec(Border.flat("#ff0000", 1), 0).widths.top, 1)

    var surface = createTemporaryObject(crispSurfaceComponent, testCase, {
      devicePixelRatio: 2,
      sourceBorderSpec: Border.flat("#00ff00", 0.4)
    })
    verify(surface !== null)
    compare(Border.top(surface.borderSpec), 0.5)
    compare(Border.bottom(surface.borderSpec), 0.5)

    var field = createTemporaryObject(crispTextFieldComponent, testCase, {
      devicePixelRatio: 3
    })
    verify(field !== null)
    compare(field.background.devicePixelRatio, 3)
  }

  function test_avatar_initials_fallback_and_preview_requests() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var avatar = createTemporaryObject(avatarComponent, testCase, {
      service: service,
      jid: "alice@s.whatsapp.net",
      name: "Alice Smith",
      size: 40,
      imageObjectName: "componentAvatarImage"
    })
    verify(avatar !== null)
    compare(avatar.width, 40)
    compare(avatar.height, 40)
    compare(avatar.radius, 20)
    compare(avatar.initials, "AS")
    compare(avatar.hasRenderedAvatar, false)

    var image = findChild(avatar, "componentAvatarImage")
    verify(image !== null)
    compare(String(image.source), "")
    compare(image.fillMode, Image.PreserveAspectCrop)

    service.avatarUrls = ({
      "alice@s.whatsapp.net": String(Qt.resolvedUrl("fixtures/pixel.svg"))
    })
    tryCompare(avatar, "hasRenderedAvatar", true)
    service.avatarUrls = ({})
    tryCompare(avatar, "hasRenderedAvatar", false)

    var placeholder = createTemporaryObject(avatarComponent, testCase, {})
    verify(placeholder !== null)
    compare(placeholder.initials, "?")
    compare(placeholder.hasRenderedAvatar, false)

    compare(service.calls.length, 0)
    var voter = createTemporaryObject(avatarComponent, testCase, {
      service: service,
      jid: "carol@s.whatsapp.net",
      requestOnLoad: true
    })
    verify(voter !== null)
    compare(service.calls.length, 1)
    compare(service.calls[0].name, "requestAvatar")
    compare(service.calls[0].value, "carol@s.whatsapp.net")
  }

  function test_document_card_opens_and_saves_the_cached_file() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var card = createTemporaryObject(documentCardComponent, testCase, {
      service: service,
      media: {
        kind: "document",
        path: "/synthetic/media/report.pdf",
        file_name: "report.pdf",
        mime_type: "application/pdf",
        file_size: 2048,
        page_count: 3
      }
    })
    verify(card !== null)
    compare(card.openButton.tooltipText, "Open document")
    compare(card.saveButton.tooltipText, "Save to Downloads")

    card.openButton.click()
    compare(lastCall(service, "openFile").value, "/synthetic/media/report.pdf")
    card.saveButton.click()
    compare(lastCall(service, "saveFile").value, "/synthetic/media/report.pdf")
  }

  function test_location_card_reports_live_share_deadline() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var panel = createTemporaryObject(panelComponent, testCase, {
      service: service
    })
    var card = createTemporaryObject(locationCardComponent, testCase, {
      panel: panel,
      service: service,
      messageTimestamp: 1000,
      media: {
        kind: "location",
        live: true,
        updated_at: 1000,
        duration_seconds: 600,
        name: "Office",
        address: "Main street 1",
        latitude_e7: 521234567,
        longitude_e7: 43210987
      }
    })
    verify(card !== null)
    compare(card.liveUntil, 1600)
    compare(findChild(card, "locationName").text, "Office")
    compare(findChild(card, "locationAddress").text, "Main street 1")
    compare(findChild(card, "locationLiveRemaining").text,
      panel.remainingTimeLabel(1600))

    card.media = Object.assign({}, card.media, { live_until: 4000 })
    compare(card.liveUntil, 4000)
    card.media = Object.assign({}, card.media, { live: false })
    compare(card.liveUntil, 0)
    compare(findChild(card, "locationLiveRemaining").text, "")

    card.openMapButton.click()
    var mapCall = lastCall(service, "openMap")
    compare(mapCall.value.latitude, 521234567)
    compare(mapCall.value.longitude, 43210987)
  }

  function test_sticker_card_prefers_the_downloaded_file() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var preview = decodeURIComponent(
      String(Qt.resolvedUrl("fixtures/pixel.svg")).substring("file://".length))
    var card = createTemporaryObject(stickerCardComponent, testCase, {
      service: service,
      active: true,
      aspectRatio: 1,
      message: { id: "sticker-component" },
      media: {
        kind: "sticker",
        path: preview + "#downloaded",
        thumbnail_path: preview,
        downloaded: false,
        lottie: false
      },
      statusObjectName: "componentStickerStatus"
    })
    verify(card !== null)
    compare(card.downloaded, false)
    compare(card.lottie, false)
    compare(card.displayPath, preview)
    compare(findChild(card, "componentStickerStatus").active, false)

    service.downloadMedia({ id: "sticker-component" })
    compare(findChild(card, "componentStickerStatus").active, true)

    card.media = Object.assign({}, card.media, { downloaded: true })
    compare(card.displayPath, preview + "#downloaded")
    compare(findChild(card, "componentStickerStatus").active, false)

    card.media = Object.assign({}, card.media, { lottie: true })
    compare(card.lottie, true)
  }

  function test_voice_message_card_labels_audio_and_duration() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var panel = createTemporaryObject(panelComponent, testCase, {
      service: service
    })
    var card = createTemporaryObject(voiceMessageCardComponent, testCase, {
      panel: panel,
      service: service,
      message: { id: "voice-component" },
      media: {
        kind: "audio",
        path: "/synthetic/media/voice.ogg",
        downloaded: true,
        duration_seconds: 65
      }
    })
    verify(card !== null)
    compare(card.downloaded, true)
    compare(card.totalSeconds, 65)
    compare(card.active, false)
    compare(card.playing, false)
    compare(card.progress, 0)
    compare(findChild(card, "voiceMessageTitle").text, "Voice message")
    compare(findChild(card, "voiceMessageDuration").text, "1:05")
    compare(card.playButton.tooltipText, "Play voice message")

    card.media = Object.assign({}, card.media, { voice_message: false })
    compare(findChild(card, "voiceMessageTitle").text, "Audio")
    card.media = Object.assign({}, card.media, { downloaded: false })
    compare(card.playButton.tooltipText, "Download voice message")
  }

  function test_media_preview_card_classifies_its_media() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var card = createTemporaryObject(mediaPreviewCardComponent, testCase, {
      service: service,
      message: { id: "media-component" },
      mediaAspectRatio: 16 / 9,
      media: {
        kind: "video",
        gif_playback: true,
        path: "/synthetic/media/clip.mp4",
        thumbnail_path: "/synthetic/media/clip.png",
        downloaded: false,
        width: 16,
        height: 9
      }
    })
    verify(card !== null)
    compare(card.isVideo, true)
    compare(card.isGif, true)
    compare(card.isImage, false)
    compare(card.topMargin, Style.space(8))
    // A video always shows its cached thumbnail, downloaded or not.
    compare(card.displayPath, "/synthetic/media/clip.png")
    compare(card.imageAspectRatio, 16 / 9)

    card.media = Object.assign({}, card.media, {
      kind: "image",
      gif_playback: false,
      downloaded: true
    })
    compare(card.isImage, true)
    compare(card.isVideo, false)
    compare(card.displayPath, "/synthetic/media/clip.mp4")
    card.media = Object.assign({}, card.media, { downloaded: false })
    compare(card.displayPath, "/synthetic/media/clip.png")
  }

  function test_media_preview_card_sizes_from_announced_dimensions() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var card = createTemporaryObject(mediaPreviewCardComponent, testCase, {
      service: service,
      message: { id: "size-component" },
      mediaAspectRatio: 2,
      media: {
        kind: "video",
        path: "/synthetic/media/clip.mp4",
        thumbnail_path: "/synthetic/media/clip.png",
        downloaded: false,
        width: 16,
        height: 8
      }
    })
    verify(card !== null)
    card.width = 160
    // Test doubles are never shown, so ancestors stay invisible: the height
    // must still follow the announced dimensions rather than the effective
    // visibility.
    compare(card.height, Style.space(8) + 160 / 2)
    card.topMargin = 0
    compare(card.height, 160 / 2)
    card.media = Object.assign({}, card.media, {
      kind: "image",
      downloaded: true
    })
    compare(card.height, 160 / 2)
    card.media = null
    compare(card.height, 0)
  }

  function test_media_preview_card_marks_high_definition_images() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var card = createTemporaryObject(mediaPreviewCardComponent, testCase, {
      service: service,
      message: { id: "hd-component" },
      hdBadgeObjectName: "hdBadge-hd-component",
      media: {
        kind: "image",
        path: "/synthetic/media/photo.jpg",
        thumbnail_path: "/synthetic/media/thumb.jpg",
        downloaded: true,
        width: 4000,
        height: 3000
      }
    })
    verify(card !== null)
    compare(card.showHdBadge, true)
    verify(findChild(card, "hdBadge-hd-component") !== null)
    card.media = Object.assign({}, card.media, { width: 800, height: 600 })
    compare(card.showHdBadge, false)
  }

  function test_media_preview_card_opens_full_size_viewer() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var opened = []
    var panelDouble = {
      openVideoPreview: function (path, isGif) {
        opened.push({ kind: "video", path: path, isGif: isGif })
      },
      openImagePreview: function (path, revision, width, height) {
        opened.push({ kind: "image", path: path, revision: revision, width: width, height: height })
      }
    }
    var card = createTemporaryObject(mediaPreviewCardComponent, testCase, {
      service: service,
      panel: panelDouble,
      message: { id: "open-component" },
      media: {
        kind: "video",
        gif_playback: true,
        path: "/synthetic/media/clip.mp4",
        thumbnail_path: "/synthetic/media/clip.png",
        downloaded: true,
        width: 16,
        height: 9
      }
    })
    verify(card !== null)
    card.openPreview()
    compare(opened.length, 1)
    compare(opened[0].kind, "video")
    compare(opened[0].path, "/synthetic/media/clip.mp4")
    compare(opened[0].isGif, true)

    card.media = Object.assign({}, card.media, {
      kind: "image",
      gif_playback: false
    })
    card.openPreview()
    compare(opened.length, 2)
    compare(opened[1].kind, "image")
    compare(opened[1].path, "/synthetic/media/clip.mp4")
    compare(opened[1].revision, "0-0")
    compare(opened[1].width, 16)
    compare(opened[1].height, 9)
  }

  function test_album_mosaic_layouts_two_three_and_four_tiles() {
    var service = createTemporaryObject(serviceComponent, testCase)
    function syntheticTile(index) {
      return {
        id: "album-layout-" + index,
        media: {
          kind: "image",
          path: "/synthetic/media/photo-" + index + ".jpg",
          thumbnail_path: "/synthetic/media/thumb-" + index + ".jpg",
          downloaded: true,
          width: 4000,
          height: 3000
        }
      }
    }

    var two = createTemporaryObject(albumMosaicComponent, testCase, {
      service: service,
      width: 318
    })
    two.messages = [syntheticTile(0), syntheticTile(1)]
    verify(two !== null)
    compare(two.cellSize, 158)
    compare(two.height, 158)
    compare(findChild(two, "albumTile-album-layout-0").showHdBadge, true)
    compare(findChild(two, "albumTile-album-layout-0").x, 0)
    compare(findChild(two, "albumTile-album-layout-0").width, 158)
    compare(findChild(two, "albumTile-album-layout-1").x, 160)
    compare(findChild(two, "albumTile-album-layout-1").y, 0)

    var three = createTemporaryObject(albumMosaicComponent, testCase, {
      service: service,
      width: 318
    })
    three.messages = [syntheticTile(0), syntheticTile(1), syntheticTile(2)]
    compare(three.height, 318)
    compare(findChild(three, "albumTile-album-layout-0").height, 318)
    compare(findChild(three, "albumTile-album-layout-1").x, 160)
    compare(findChild(three, "albumTile-album-layout-1").y, 0)
    compare(findChild(three, "albumTile-album-layout-2").x, 160)
    compare(findChild(three, "albumTile-album-layout-2").y, 160)

    var four = createTemporaryObject(albumMosaicComponent, testCase, {
      service: service,
      width: 318
    })
    four.messages = [syntheticTile(0), syntheticTile(1), syntheticTile(2), syntheticTile(3)]
    compare(four.height, 318)
    compare(findChild(four, "albumTile-album-layout-3").x, 160)
    compare(findChild(four, "albumTile-album-layout-3").y, 160)

    var single = createTemporaryObject(albumMosaicComponent, testCase, {
      service: service,
      width: 318
    })
    single.messages = [syntheticTile(0)]
    compare(single.visible, false)
    compare(single.height, 0)
  }

  function test_album_mosaic_opens_and_downloads_per_tile() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var opened = []
    var downloads = []
    var panelDouble = {
      openVideoPreview: function (path, isGif) {
        opened.push({ kind: "video", path: path, isGif: isGif })
      },
      openImagePreview: function (path, revision, width, height) {
        opened.push({ kind: "image", path: path, width: width, height: height })
      },
      downloadMedia: function (message) {
        downloads.push(message)
      }
    }
    var members = [
      {
        id: "album-tile-image",
        media: {
          kind: "image",
          path: "/synthetic/media/photo.jpg",
          thumbnail_path: "/synthetic/media/thumb.jpg",
          downloaded: true,
          width: 800,
          height: 600
        }
      },
      {
        id: "album-tile-video",
        media: {
          kind: "video",
          gif_playback: true,
          path: "/synthetic/media/clip.mp4",
          thumbnail_path: "/synthetic/media/thumb.jpg",
          downloaded: true
        }
      },
      {
        id: "album-tile-pending",
        media: {
          kind: "image",
          path: "",
          thumbnail_path: "/synthetic/media/thumb.jpg",
          downloaded: false
        }
      }
    ]
    var mosaic = createTemporaryObject(albumMosaicComponent, testCase, {
      service: service,
      panel: panelDouble,
      width: 318
    })
    mosaic.messages = members
    verify(mosaic !== null)
    compare(findChild(mosaic, "albumTile-album-tile-image").showHdBadge,
      false)
    compare(findChild(mosaic, "albumTile-album-tile-image").showDownloadButton,
      false)
    compare(findChild(mosaic, "albumTile-album-tile-video").showDownloadButton,
      false)
    var pendingButton = findChild(mosaic, "albumDownloadButton-album-tile-pending")
    compare(findChild(mosaic, "albumTile-album-tile-pending").showDownloadButton,
      true)
    compare(pendingButton.tooltipText, "Download media")
    pendingButton.click()
    compare(downloads.length, 1)
    compare(downloads[0].id, "album-tile-pending")

    mosaic.openTile(members[1])
    compare(opened.length, 1)
    compare(opened[0].kind, "video")
    compare(opened[0].path, "/synthetic/media/clip.mp4")
    compare(opened[0].isGif, true)
    mosaic.openTile(members[0])
    compare(opened.length, 2)
    compare(opened[1].kind, "image")
    compare(opened[1].path, "/synthetic/media/photo.jpg")
    compare(opened[1].width, 800)
    compare(opened[1].height, 600)
    mosaic.openTile(members[2])
    compare(downloads.length, 2)
    compare(downloads[1].id, "album-tile-pending")
  }

  function test_poll_card_replaces_or_extends_the_selection() {
    var service = createTemporaryObject(serviceComponent, testCase)
    var panel = createTemporaryObject(panelComponent, testCase, {
      service: service
    })
    var options = [
      { name: "Soup", votes: 1, selected_by_me: false, voter_jids: [] },
      { name: "Salad", votes: 1, selected_by_me: true, voter_jids: [] }
    ]
    var card = createTemporaryObject(pollCardComponent, testCase, {
      panel: panel,
      service: service,
      active: true,
      messageId: "poll-component",
      message: { id: "poll-component" },
      media: {
        kind: "poll",
        question: "Lunch?",
        selectable_count: 1,
        total_voters: 2,
        end_timestamp: 0,
        options: options
      }
    })
    verify(card !== null)
    compare(card.ended, false)
    compare(card.totalVoters, 2)
    compare(findChild(card, "pollQuestionLabel").text, "Lunch?")
    compare(findChild(card, "pollSelectionHint").text, "Select one")
    compare(findChild(card, "pollVoteTotal").text, "2 votes")
    compare(card.selectedPollOptions().length, 1)
    compare(card.selectedPollOptions()[0], "Salad")

    // A single-answer poll replaces the previous selection.
    card.togglePollOption(0)
    compare(service.pollVotes.length, 1)
    compare(service.pollVotes[0].selectedOptions.length, 1)
    compare(service.pollVotes[0].selectedOptions[0], "Soup")

    card.media = Object.assign({}, card.media, { selectable_count: 2 })
    compare(findChild(card, "pollSelectionHint").text, "Select one or more")
    card.togglePollOption(0)
    compare(service.pollVotes.length, 2)
    compare(service.pollVotes[1].selectedOptions.length, 2)
    compare(service.pollVotes[1].selectedOptions[0], "Salad")
    compare(service.pollVotes[1].selectedOptions[1], "Soup")

    // An unnamed option cannot be voted for, and an ended poll accepts nothing.
    card.media = Object.assign({}, card.media, {
      options: [{ name: "", votes: 0, selected_by_me: false, voter_jids: [] }]
    })
    card.togglePollOption(0)
    compare(service.pollVotes.length, 2)

    card.media = Object.assign({}, card.media, {
      end_timestamp: 1,
      total_voters: 1,
      options: options
    })
    compare(card.ended, true)
    compare(findChild(card, "pollVoteTotal").text, "Poll ended")
    card.togglePollOption(0)
    compare(service.pollVotes.length, 2)
  }

  function test_reaction_picker_hands_back_a_pasted_emoji() {
    var chosen = []
    var picker = createTemporaryObject(reactionPickerComponent, testCase, {
      ownReactionEmoji: "👍"
    })
    verify(picker !== null)
    picker.reactionChosen.connect(function (emoji) {
      chosen.push(emoji)
    })
    compare(findChild(picker, "reactionPickerHint").text,
      "Tap selected to remove")
    picker.ownReactionEmoji = ""
    compare(findChild(picker, "reactionPickerHint").text, "Choose one")
    compare(findChild(picker, "reactionPickerEmojiButton").text,
      "Choose any emoji")

    // Text that arrives without an outstanding request is not a reaction.
    picker.pasteTarget.text = "🎉"
    wait(20)
    compare(chosen.length, 0)

    picker.waitingForEmojiPicker = true
    compare(findChild(picker, "reactionPickerEmojiButton").text,
      "Choose an emoji…")
    picker.pasteTarget.text = "🎊"
    compare(picker.waitingForEmojiPicker, false)
    tryVerify(function () {
      return chosen.length === 1
    })
    compare(chosen[0], "🎊")
  }

  function test_receipt_tooltip_renders_one_section_per_group() {
    var tooltip = createTemporaryObject(receiptTooltipComponent, testCase, {
      text: "Read\nAlice\n\nDelivered\nBob",
      groups: [
        { label: "Read", entries: ["Alice"] },
        { label: "Delivered", entries: ["Bob"] }
      ]
    })
    verify(tooltip !== null)
    compare(tooltip.delay, 400)
    compare(tooltip.timeout, -1)
    compare(tooltip.padding, 0)
    compare(tooltip.background.color, Color.tooltip.background)
    compare(tooltip.background.radius, 0)
    compare(tooltip.contentItem.groups.length, 2)
    compare(tooltip.contentItem.groups[0].label, "Read")
    compare(tooltip.contentItem.groups[1].entries[0], "Bob")
    compare(tooltip.contentItem.detailColor, Color.tooltip.text)
    compare(tooltip.contentItem.headerColor, Qt.rgba(
      Color.tooltip.text.r, Color.tooltip.text.g, Color.tooltip.text.b,
      Color.tooltip.text.a * 0.72))
    compare(tooltip.contentItem.leftInset,
      Border.left(tooltip.tooltipBorderSpec) + Style.spacing.controlPaddingX)
    compare(tooltip.contentItem.topInset,
      Border.top(tooltip.tooltipBorderSpec) + Style.spacing.controlPaddingY)
  }

  function test_bar_widget_loads_and_formats_state() {
    var widget = createTemporaryObject(barComponent, testCase)
    verify(widget !== null)
    compare(widget.unread, 0)
    compare(widget.connectionState, "starting")
    compare(widget.showCount, true)
    compare(widget.hideWhenEmpty, false)
    compare(widget.unreadLabel, "0")

    widget.settings = { showUnreadCount: false, hideWhenEmpty: true }
    compare(widget.showCount, false)
    compare(widget.hideWhenEmpty, true)
  }
}
