"""Fake-only controls for the private enrollment boundary."""
import asyncio
import contextlib
import io
import importlib.util
import pathlib
import sys
import unittest
import uuid


class EnrollmentTests(unittest.TestCase):
    def setUp(self):
        path = pathlib.Path(__file__).with_name("private-enrollment.py")
        spec = importlib.util.spec_from_file_location("private_enrollment", path)
        self.module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = self.module
        spec.loader.exec_module(self.module)

    def test_private_broker_value_never_enters_public_result(self):
        m = self.module
        packet, presence, receipt = uuid.uuid4(), uuid.uuid4(), uuid.uuid4()
        private_value = "synthetic-provider-confirmation-never-public"

        class Scope:
            async def authorize(self, human, requested):
                return m.ApprovedScope(requested, human.employee_id, presence, "account-fixture")

        class Broker:
            calls = 0

            async def begin_private(self, scope):
                self.calls += 1
                self.private_value = private_value
                return m.OpaqueReceipt(receipt)

        broker = Broker()
        output, errors = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            result = asyncio.run(m.begin(m.Human("employee-fixture"), packet, Scope(), broker))
        self.assertEqual(result.public_record(), {"broker_receipt_id": str(receipt)})
        self.assertNotIn(private_value, str(result.public_record()))
        self.assertEqual(broker.calls, 1)
        self.assertEqual(output.getvalue(), "")
        self.assertEqual(errors.getvalue(), "")

    def test_refusals_are_fixed_and_do_not_call_provider(self):
        m = self.module
        packet = uuid.uuid4()

        class Scope:
            async def authorize(self, human, requested):
                return m.ApprovedScope(uuid.uuid4(), human.employee_id, uuid.uuid4(), "account-fixture")

        class Broker:
            calls = 0

            async def begin_private(self, scope):
                self.calls += 1
                raise RuntimeError("synthetic-private-provider-failure")

        broker = Broker()
        result = asyncio.run(m.begin(m.Human("employee-fixture"), packet, Scope(), broker))
        self.assertEqual(result.public_record(), {"refusal": "scope_mismatch"})
        self.assertEqual(broker.calls, 0)
        result = asyncio.run(m.begin(m.Human(""), packet, Scope(), broker))
        self.assertEqual(result.public_record(), {"refusal": "human_required"})

    def test_private_exception_and_uninstalled_adapter_stay_unavailable(self):
        m = self.module
        packet = uuid.uuid4()

        class Scope:
            async def authorize(self, human, requested):
                return m.ApprovedScope(requested, human.employee_id, uuid.uuid4(), "account-fixture")

        class Broker:
            async def begin_private(self, scope):
                raise RuntimeError("synthetic-private-provider-failure")

        for broker in (Broker(), m.UninstalledBroker()):
            result = asyncio.run(m.begin(m.Human("employee-fixture"), packet, Scope(), broker))
            self.assertEqual(result.public_record(), {"refusal": "private_broker_unavailable"})
            self.assertNotIn("synthetic-private", str(result.public_record()))

    def test_untrusted_receipt_and_scope_fail_closed(self):
        m = self.module
        packet = uuid.uuid4()

        class Scope:
            async def authorize(self, human, requested):
                return m.ApprovedScope(requested, human.employee_id, uuid.uuid4(), "account-fixture")

        class Broker:
            async def begin_private(self, scope):
                return m.OpaqueReceipt("synthetic-private-value")

        result = asyncio.run(m.begin(m.Human("employee-fixture"), packet, Scope(), Broker()))
        self.assertEqual(result.public_record(), {"refusal": "private_broker_unavailable"})

        class FailedScope:
            async def authorize(self, human, requested):
                raise RuntimeError("synthetic-private-authorization-value")

        result = asyncio.run(m.begin(m.Human("employee-fixture"), packet, FailedScope(), Broker()))
        self.assertEqual(result.public_record(), {"refusal": "scope_unavailable"})

    def test_nil_receipt_cannot_claim_a_private_effect(self):
        m = self.module

        class Scope:
            async def authorize(self, human, requested):
                return m.ApprovedScope(requested, human.employee_id, uuid.uuid4(), "account-fixture")

        class Broker:
            async def begin_private(self, scope):
                return m.OpaqueReceipt(uuid.UUID(int=0))

        result = asyncio.run(m.begin(m.Human("employee-fixture"), uuid.uuid4(), Scope(), Broker()))
        self.assertEqual(result.public_record(), {"refusal": "private_broker_unavailable"})

    def test_interrupted_private_flow_does_not_export_its_exception(self):
        m = self.module

        class Scope:
            async def authorize(self, human, requested):
                return m.ApprovedScope(requested, human.employee_id, uuid.uuid4(), "account-fixture")

        class Broker:
            async def begin_private(self, scope):
                raise asyncio.CancelledError("synthetic-private-interruption")

        result = asyncio.run(m.begin(m.Human("employee-fixture"), uuid.uuid4(), Scope(), Broker()))
        self.assertEqual(result.public_record(), {"refusal": "private_broker_unavailable"})

    def test_missing_presence_never_calls_the_private_broker(self):
        m = self.module

        class Scope:
            async def authorize(self, human, requested):
                return m.ApprovedScope(requested, human.employee_id, uuid.UUID(int=0), "account-fixture")

        class Broker:
            async def begin_private(self, scope):
                raise AssertionError("missing presence reached private broker")

        result = asyncio.run(m.begin(m.Human("employee-fixture"), uuid.uuid4(), Scope(), Broker()))
        self.assertEqual(result.public_record(), {"refusal": "scope_mismatch"})

    def test_malformed_private_scope_never_formats_its_payload(self):
        m = self.module

        class PrivatePayload:
            def __eq__(self, other):
                raise RuntimeError("synthetic-private-scope-value")

        class Scope:
            async def authorize(self, human, requested):
                return m.ApprovedScope(PrivatePayload(), human.employee_id, uuid.uuid4(), "account-fixture")

        result = asyncio.run(m.begin(m.Human("employee-fixture"), uuid.uuid4(), Scope(), m.UninstalledBroker()))
        self.assertEqual(result.public_record(), {"refusal": "scope_mismatch"})


if __name__ == "__main__":
    unittest.main()
