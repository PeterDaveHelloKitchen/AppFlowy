import 'dart:async';
import 'dart:typed_data';
import 'package:appflowy_backend/protobuf/flowy-notification/protobuf.dart';
import 'package:dartz/dartz.dart';
import 'package:appflowy_backend/protobuf/flowy-error/errors.pb.dart';
import 'package:appflowy_backend/protobuf/flowy-database2/notification.pb.dart';
import 'package:appflowy_backend/rust_stream.dart';

import 'notification_helper.dart';

// DatabasePB
typedef DatabaseNotificationCallback = void Function(
  DatabaseNotification,
  Either<Uint8List, FlowyError>,
);

class DatabaseNotificationParser
    extends NotificationParser<DatabaseNotification, FlowyError> {
  DatabaseNotificationParser({
    super.id,
    required super.callback,
  }) : super(
          tyParser: (ty) => DatabaseNotification.valueOf(ty),
          errorParser: (bytes) => FlowyError.fromBuffer(bytes),
        );
}

typedef DatabaseNotificationHandler = Function(
  DatabaseNotification ty,
  Either<Uint8List, FlowyError> result,
);

class DatabaseNotificationListener {
  DatabaseNotificationListener({
    required String objectId,
    required DatabaseNotificationHandler handler,
  }) : _parser = DatabaseNotificationParser(id: objectId, callback: handler) {
    _subscription =
        RustStreamReceiver.listen((observable) => _parser?.parse(observable));
  }

  DatabaseNotificationParser? _parser;
  StreamSubscription<SubscribeObject>? _subscription;

  Future<void> stop() async {
    _parser = null;
    await _subscription?.cancel();
    _subscription = null;
  }
}
